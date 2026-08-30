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
    assert!(
        per_byte < 50,
        "a plain string byte cost {per_byte} steps to quote; it was ~117"
    );
}

#[test]
fn small_objects_have_a_known_price() {
    let build = steps(&format!("{OBJECTS} return a.length;"));
    let ser = steps(&format!("{OBJECTS} return JSON.stringify(a).length;"));
    let len = 50 * 40; // ~ {"name":"item12","count":12,"ok":true},
    let per_byte = (ser - build) / len;
    println!(
        "JSON.stringify of 50 small objects: {} steps, ~{per_byte} per output byte",
        ser - build
    );
    assert!(
        per_byte < 400,
        "a small-object byte cost {per_byte} steps to serialize"
    );
}

#[test]
fn quoting_keeps_every_escape_around_the_runs() {
    for (source, want) in [
        (r#"return JSON.stringify("plain");"#, r#""plain""#),
        (r#"return JSON.stringify("");"#, r#""""#),
        (r#"return JSON.stringify("a\"b\\c\nd");"#, r#""a\"b\\c\nd""#),
        (r#"return JSON.stringify("\"");"#, r#""\"""#),
        (r#"return JSON.stringify("\u0001x\ty");"#, r#""\u0001x\ty""#),
        (
            r#"return JSON.stringify("héllo wörld");"#,
            r#""héllo wörld""#,
        ),
        (r#"return JSON.stringify("end\\");"#, r#""end\\""#),
        (r#"return JSON.stringify({k: "v\"q"});"#, r#"{"k":"v\"q"}"#),
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

#[test]
fn a_flat_object_of_thirty_properties_has_a_known_price() {
    let props: Vec<String> = (0..30).map(|i| format!("k{i:02}: \"v{i}\"")).collect();
    let build = format!("let o = {{{}}};", props.join(", "));
    let base = steps(&format!("{build} return 1;"));
    let one = steps(&format!("{build} return JSON.stringify(o).length;"));
    let per = (one - base) / 30;
    println!(
        "JSON.stringify of a 30-property flat object: {} steps, {per} per property",
        one - base
    );
    assert!(per < 1_500, "a flat property cost {per} steps to serialize");
}

#[test]
fn a_journal_record_has_a_known_price() {
    // `test_harness.qjs::append_command_record`'s record: a 13-digit
    // millisecond timestamp, an argument list, and a bounded `output` that
    // is itself pretty JSON, so a quote or a newline every twenty bytes.
    // server-smoke writes 34 of them three times each (journal, `[record]`,
    // and the folded log): 7.05M steps, 23% of the journey (2026-08-30).
    // 82k a record then; 51k on 2026-08-31, once the timestamp's digits
    // stopped going through Dragon4.
    let record = r#"let record = { recorded_at_ms: 1788101296722, arguments: ["ui-lease", "attach", "--client-id", "server-smoke-ui-1788101296722-78157", "--client-pid", "78157"], expected_failure: false, exit_code: 0, output: "{\n  \"schema_version\": 2,\n  \"lease_id\": \"ui-1314e-1a0532483bb-1\",\n  \"client_id\": \"server-smoke-ui-1788101296722-78157\",\n  \"client_pid\": 78157,\n  \"server_pid\": 78158,\n  \"position\": {\n    \"server_epoch\": \"78158-1788101296-732145000\",\n    \"sequence\": 4\n  },\n  \"expires_unix_ms\": 1788101302083,\n  \"ttl_ms\": 5000,\n  \"observed_sequence\": 0\n}\n" };"#;
    let base = steps(&format!("{record} return record.exit_code;"));
    let one = steps(&format!("{record} return JSON.stringify(record).length;"));
    println!("JSON.stringify of a journal record: {} steps", one - base);
    assert!(
        one - base < 60_000,
        "a journal record cost {} steps; it was 51k",
        one - base
    );
    // The timestamp alone: 32 567 while an integer past the i32 range left
    // the digit loop for the general double-to-string path; 541 since the
    // loop covers the safe-integer range (2026-08-31).
    let small = steps("let o = {a: 1}; return JSON.stringify(o).length;");
    let big = steps("let o = {a: 1788101436756}; return JSON.stringify(o).length;");
    println!(
        "a 13-digit integer costs {} steps more than `1` to serialize",
        big - small
    );
    assert!(
        big - small < 1_200,
        "a millisecond timestamp cost {} steps; it was 541",
        big - small
    );
}

/// What a gap adds: the same 50 small objects with `space = 2`. Every
/// property is a line feed, a comma and the indentation, so the pretty
/// answer is longer and the extra is priced per output byte too; the
/// compact answer must not have moved (the gap test is one field read per
/// object and one per property).
#[test]
fn a_gap_costs_its_own_bytes_and_nothing_on_the_compact_path() {
    let build = steps(&format!("{OBJECTS} return a.length;"));
    let compact = steps(&format!("{OBJECTS} return JSON.stringify(a).length;"));
    let pretty = steps(&format!(
        "{OBJECTS} return JSON.stringify(a, null, 2).length;"
    ));
    // Compact: 50 * ~40 bytes. Pretty: five lines an object, indented
    // two and four -- ~92 bytes an object.
    let per_compact = (compact - build) / (50 * 40);
    let per_pretty = (pretty - build) / (50 * 92);
    println!(
        "JSON.stringify of 50 small objects: compact {} steps (~{per_compact}/byte), \
         space = 2 {} steps (~{per_pretty}/byte)",
        compact - build,
        pretty - build
    );
    assert!(
        per_compact < 100,
        "compact cost {per_compact} a byte; it was ~89"
    );
    assert!(per_pretty < 100, "pretty cost {per_pretty} a byte");
}
