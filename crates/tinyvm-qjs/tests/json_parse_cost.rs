//! What `JSON.parse` costs per kind of content, pinned so it cannot creep.
//!
//! Measured through the downstream CLI at 904a22ee: ~270 steps per byte of
//! short integers (~1 600 per five-digit number: the accepted text was
//! copied into a record and parsed a second time by `__str_to_num`), ~84-119
//! per byte of string (one `__jb_byte` call per character), ~200 per byte of
//! small objects. A 393 KiB catalog could not be parsed under 100M steps.

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

fn doc(item: &str, n: usize) -> String {
    format!(
        "let t = \"[\"; for (let i = 0; i < {n}; i = i + 1) {{ t = t + \"{item}\"; if (i < {n} - 1) {{ t = t + \",\"; }} }} t = t + \"]\";"
    )
}

fn parse_cost(item: &str, n: usize) -> u64 {
    let build = steps(&format!("{} return t.length;", doc(item, n)));
    let parse = steps(&format!("{} return JSON.parse(t).length;", doc(item, n)));
    parse - build
}

#[test]
fn short_integers_are_read_in_one_pass() {
    let per = parse_cost("12345", 100) / 100;
    println!("JSON.parse of \"12345\": {per} steps each");
    // 527 measured against a baseline whose `t.length` cost ~30 steps a
    // byte; with `.length` counted by the word (2026-08-30) the baseline
    // lost ~18 000 steps and the same parse reads ~700. One `__jp_at` call
    // per digit is what is left.
    assert!(
        per < 800,
        "a five-digit integer cost {per} steps; it was ~1 600"
    );
}

#[test]
fn short_fractions_are_one_exact_division_and_exponents_take_the_general_path() {
    let per = parse_cost("1.5", 100) / 100;
    println!("JSON.parse of \"1.5\": {per} steps each");
    assert!(per < 800, "\"1.5\" cost {per} steps; it was ~1 300");
    for (text, want) in [
        ("[0.1]", 0.1f64),
        ("[12.375]", 12.375),
        ("[-0.5]", -0.5),
        ("[1e3]", 1000.0),
        ("[1.5e-1]", 0.15),
        ("[123456789012345.6]", 123456789012345.6),
        ("[0.30000000000000004]", 0.30000000000000004),
    ] {
        let wasm = compile_qjs_m1(&format!("return JSON.parse(\"{text}\")[0];")).expect("compiles");
        let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        let vals = instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("runs");
        assert_eq!(
            Value::returned(&vals).expect("value"),
            Value::Number(want),
            "{text}"
        );
    }
    let wasm = compile_qjs_m1(r#"return JSON.parse("[1.5, -0, 12345678901234567, 1e3]")[2];"#)
        .expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("runs");
    assert_eq!(
        Value::returned(&vals).expect("value"),
        Value::Number(12345678901234568.0)
    );
}

#[test]
fn plain_ascii_strings_are_copied_in_runs() {
    // One long string: the copy dominates, not the per-string buffer. Ten
    // ten-character strings measure ~70 steps a byte because each pays a
    // buffer, a take and an array push; that is a different price.
    let build = steps(
        r#"let t = "\""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789"; } t = t + "\""; return t.length;"#,
    );
    let parse = steps(
        r#"let t = "\""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789"; } t = t + "\""; return JSON.parse(t).length;"#,
    );
    let per_byte = (parse - build) / 1002;
    println!("JSON.parse of a 1000-char string: {per_byte} steps per byte");
    assert!(
        per_byte < 40,
        "a plain string byte cost {per_byte} steps; it was ~119"
    );
}

fn lit(text: &str) -> String {
    let mut s = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// What parsing `text` costs, with the literal's own cost taken out.
fn parse_cost_of(text: &str) -> u64 {
    let l = lit(text);
    let base = steps(&format!("let t = {l}; return t.length;"));
    let parse = steps(&format!("let t = {l}; return typeof JSON.parse(t);"));
    parse - base
}

/// A broker answer the way `agenterm cli ... --json` prints one: pretty,
/// two-space indented, objects of short keys with string, number and boolean
/// values, a list of them under one key -- `ui-bootstrap`'s shape. Sixty
/// percent of its bytes are indentation.
fn pretty_answer(tabs: usize) -> String {
    let mut s =
        String::from("{\n  \"schema_version\": 2,\n  \"active_tab_id\": \"@7\",\n  \"tabs\": [\n");
    for i in 0..tabs {
        s.push_str(&format!(
            "    {{\n      \"id\": \"@{i}\",\n      \"title\": \"headless-{i}\",\n      \"note\": \"\",\n      \"screen\": {{\n        \"rows\": 24,\n        \"columns\": 90,\n        \"alternate\": false\n      }},\n      \"active\": {},\n      \"pinned\": false\n    }}{}\n",
            i == 0,
            if i + 1 < tabs { "," } else { "" }
        ));
    }
    s.push_str("  ],\n  \"server_pid\": 78158\n}\n");
    s
}

#[test]
fn a_pretty_broker_answer_has_a_known_price_per_byte() {
    // server-smoke's bill, profiled 2026-08-30 (`plan/design-host-op-budget.md`
    // §7 downstream): 225 KB of such answers cost 13.56M steps, 43% of the
    // journey, at 58-64 steps a byte -- 39 of them on every indentation
    // byte, ~760 fixed on every key and string. Whitespace by the word and
    // strings built straight from the text (the same night) brought the
    // real answers to 34-42 a byte; this synthetic one reads ~49.
    let small = pretty_answer(8);
    let large = pretty_answer(200);
    let per_small = parse_cost_of(&small) / small.len() as u64;
    let per_large = parse_cost_of(&large) / large.len() as u64;
    println!(
        "JSON.parse of a pretty answer: {} bytes at {per_small}/byte, {} bytes at {per_large}/byte",
        small.len(),
        large.len()
    );
    assert!(
        per_large < 60,
        "a pretty answer byte cost {per_large} steps; it was ~72"
    );
}

#[test]
fn json_whitespace_has_a_known_price() {
    let one = parse_cost_of("1");
    let spaces = parse_cost_of(&format!("{}1", " ".repeat(1000)));
    let per_byte = (spaces - one) / 1000;
    println!("JSON.parse over 1 000 spaces: {per_byte} steps per byte");
    assert!(
        per_byte < 20,
        "a whitespace byte cost {per_byte} steps; it was 39"
    );
    // Indentation the way a pretty printer writes it: a line feed and a run
    // of spaces, taken by the word.
    let lines = parse_cost_of(&format!("{}1", "\n        ".repeat(100)));
    let per_byte = (lines - one) / 900;
    println!("JSON.parse over 100 indented lines: {per_byte} steps per byte");
    assert!(
        per_byte < 12,
        "an indentation byte cost {per_byte} steps; it was 39"
    );
}

#[test]
fn a_short_string_has_a_known_fixed_price() {
    let strings = parse_cost_of(&format!("[{}]", vec!["\"ab\""; 200].join(",")));
    let empty = parse_cost_of("[]");
    let per = (strings - empty) / 200;
    println!("JSON.parse of \"ab\" in an array: {per} steps each");
    assert!(
        per < 700,
        "a two-character string cost {per} steps to parse; it was ~760"
    );
}
