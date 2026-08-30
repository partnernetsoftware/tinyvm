//! Template literals: `` `text` ``, `` `a${x}b` ``, nesting, and the TV
//! normalisation the spec puts on the text between the backticks.
//!
//! Same discipline as `arrays_m3.rs`: every expectation is derived from
//! ECMA-262 rather than from what the implementation happens to do, and every
//! one of them **runs** -- compile -> tinyvm's load gate -> instantiate ->
//! `invoke_by_name("main")`. "It compiled" is not evidence except in the
//! refusal corpus, where not compiling is the claim.
//!
//! # What this milestone deliberately does not have
//!
//! No tagged templates (`` tag`a${b}` ``), which are a *call* with a very
//! particular argument shape -- a frozen array of the cooked strings with a
//! `raw` property -- and so need array methods and property definition this
//! engine does not have. No `String.raw`. Both are refused by name at the
//! bottom of this file.
//!
//! # Why there is no `Template` node in the AST
//!
//! ECMA-262 13.2.8.6 says a template's value is the pieces and the `ToString`
//! of each substitution, concatenated left to right; 13.15.3 says `+` with a
//! String operand is exactly that. So the parser folds a template into a
//! chain of `+` and there is nothing further to lower. The consequence this
//! file is here to pin: templates cost a template-free program **zero** bytes,
//! because they add no runtime helper that would need a gate.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Boundary, Names, Options, Value, compile_qjs_m1, compile_qjs_m1_with};

// =========================================================================
// Harness
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
}

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
fn run(source: &str) -> Out {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let value = Value::returned(&vals)
        .unwrap_or_else(|e| panic!("{source:?}: cannot read the result back: {e}"));
    match value {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => Out::Str(read_string(&instance, ptr).expect("a string record")),
    }
}

fn read_string(instance: &WasmInstance, ptr: i32) -> Result<String, String> {
    let view = instance
        .memory()
        .map_err(|e| format!("no guest memory: {}", e.message()))?;
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let header = bytes
        .get(at..at + 4)
        .ok_or_else(|| format!("string header at {ptr} is out of bounds"))?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let body = bytes
        .get(at + 4..at + 4 + len)
        .ok_or_else(|| format!("string body at {ptr} (len {len}) is out of bounds"))?;
    String::from_utf8(body.to_vec()).map_err(|_| "string is not valid UTF-8".to_string())
}

#[track_caller]
fn string(source: &str, want: &str) {
    assert_eq!(run(source), Out::Str(want.to_string()), "{source:?}");
}

#[track_caller]
fn refuses_capability(source: &str, needle: &str, boundary: Boundary) {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a capability diagnostic",
            bytes.len()
        ),
        Err(e) => {
            assert!(
                e.message.contains(needle),
                "{source:?}: want a message naming {needle:?}, got {}",
                e.message
            );
            assert_eq!(e.boundary, boundary, "{source:?}: wrong boundary");
        }
    }
}

#[track_caller]
fn refuse(source: &str) -> String {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!("{source:?} compiled to {} bytes", bytes.len()),
        Err(e) => e.message,
    }
}

// =========================================================================
// 1. No substitutions: a template is a String
// =========================================================================

#[test]
fn a_template_without_substitutions_is_the_string_it_spells() {
    string("return `abc`;", "abc");
    string("return ``;", "");
    // And it *is* a String, not a String-like: everything a string literal
    // can be used for, this can be used for.
    string("return `ab` + `cd`;", "abcd");
    string("const o = { a: 1 }; return typeof `x`;", "string");
    // A template inherits a String's properties exactly, which is one more
    // way of saying it *is* a String and not a String-like of its own: the
    // one property this engine answers, and the ones it still traps on.
    assert_eq!(run("return `ab`.length;"), Out::Number(2.0));
    assert_eq!(run("return `caf\u{e9}`.length;"), Out::Number(4.0));
    assert_eq!(run("return `a${1}b`.length;"), Out::Number(3.0));
    assert!(attempt("return `ab`.toUpperCase;").is_err());
}

#[test]
fn the_two_quote_characters_need_no_escape_inside_a_template() {
    string(r#"return `a"b'c`;"#, "a\"b'c");
    // And the backtick does, which is the one escape a template adds.
    string("return `a\\`b`;", "a`b");
    // `\$` keeps a dollar from starting a substitution.
    string("return `a\\${b}c`;", "a${b}c");
}

#[test]
fn a_template_takes_the_same_escapes_a_string_does() {
    string(r"return `a\nb`;", "a\nb");
    string(r"return `\x41`;", "A");
    string(r"return `A`;", "A");
    string(r"return `\u{1F600}`;", "\u{1F600}");
    // LineContinuation: the backslash and the terminator both vanish.
    string("return `a\\\nb`;", "ab");
}

#[test]
fn a_template_may_hold_a_line_terminator_and_a_string_may_not() {
    // ECMA-262 12.9.6: this is the one thing a template's text can do that a
    // string literal's cannot.
    string("return `a\nb`;", "a\nb");
    let message = refuse("return \"a\nb\";");
    assert!(message.contains("the line ends first"), "{message}");
}

#[test]
fn the_tv_normalises_line_terminators_to_one_lf() {
    // ECMA-262 12.9.6's TV: `\r\n` and a lone `\r` are each one `\n`, so the
    // same template means the same thing on a CRLF file and an LF one.
    string("return `a\r\nb`;", "a\nb");
    string("return `a\rb`;", "a\nb");
    // Two of them are two, and a `\n` written as an escape is untouched.
    string("return `a\r\n\r\nb`;", "a\n\nb");
    string(r"return `a\r\nb`;", "a\r\nb");
}

// =========================================================================
// 2. Substitutions
// =========================================================================

#[test]
fn a_substitution_is_the_to_string_of_what_it_evaluates_to() {
    string("return `a${1}b`;", "a1b");
    string("return `${true}`;", "true");
    string("return `${null}`;", "null");
    string("return `${undefined}`;", "undefined");
    string("return `${1.5}`;", "1.5");
    string("return `${-0}`;", "0");
    string("return `${1 / 0}`;", "Infinity");
    string("return `${0 / 0}`;", "NaN");
}

#[test]
fn adjacent_substitutions_do_not_add() {
    // The one case the desugaring has to get right: `` `${1}${2}` `` is
    // "12" and not 3. It holds because the fold starts at the head's `""`,
    // which makes the leftmost operand a String and keeps it one.
    string("return `${1}${2}`;", "12");
    string("return `${1}${2}${3}`;", "123");
    string("return `${1}`;", "1");
    // Same question with the head non-empty and the tail empty.
    string("return `n=${1}${2}`;", "n=12");
}

#[test]
fn a_substitution_holds_any_expression() {
    string("return `${1 + 2}`;", "3");
    string("return `${1 > 2}`;", "false");
    string("let x = 2; return `${x * x}`;", "4");
    string("return `${[1, 2].length}`;", "2");
    string("function f(n) { return n + 1; } return `${f(1)}`;", "2");
    string("return `${1 ? \"y\" : \"n\"}`;", "y");
    // A conditional whose branches are themselves templates.
    string("return `${1 ? `y${2}` : \"n\"}`;", "y2");
}

#[test]
fn a_substitution_may_hold_a_brace() {
    // The `}` of an object literal must not be mistaken for the one that
    // resumes the template text. The lexer counts brace depth to tell them
    // apart, and this is the test that would fail if it did not.
    string("return `${ { a: 7 }.a }`;", "7");
    string("return `${ { a: { b: 8 } }.a.b }`;", "8");
    string("return `${ (function () { return 9; })() }`;", "9");
    // A block inside a function inside a substitution, two levels of brace.
    string(
        "return `${ (function () { if (1) { return 3; } return 4; })() }`;",
        "3",
    );
}

#[test]
fn templates_nest() {
    string("return `a${`b${`c`}d`}e`;", "abcde");
    // The inner template's `}` closes the inner substitution, and the outer
    // one closes the outer -- which is why the open-template stack is a
    // stack and not a flag.
    string("let x = 1; return `${`${x}`}`;", "1");
}

#[test]
fn a_template_works_where_any_other_expression_does() {
    string("let s = `a${1}`; return s;", "a1");
    string("const o = { k: `v${1}` }; return o.k;", "v1");
    string("return [`a${1}`][0];", "a1");
    string("function f(s) { return s; } return f(`a${1}`);", "a1");
    string("return `a${1}` + `b${2}`;", "a1b2");
    string("const o = {}; o[`k${1}`] = \"v\"; return o.k1;", "v");
    string("return typeof `${1}`;", "string");
}

#[test]
fn a_template_reads_a_captured_binding() {
    // Templates and closures landed one after the other and neither knows
    // about the other; this is the assertion that they compose.
    string(
        "function mk(n) { return function () { return `n=${n}`; }; } return mk(7)();",
        "n=7",
    );
}

#[test]
fn a_template_in_the_declared_names_mode_too() {
    // The downstream product compiles with `Names::Declared`, so a feature
    // that only worked under the default would not be reachable from
    // `agenterm-qjswasm`.
    let wasm = compile_qjs_m1_with(
        "let tab = 3; return `{\"tab\":${tab}}`;",
        Options {
            names: Names::Declared(Vec::new()),
        },
    )
    .expect("declared names compile a template");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("run");
    let Value::String(ptr) = Value::returned(&vals).expect("a value") else {
        panic!("expected a string");
    };
    assert_eq!(read_string(&instance, ptr).expect("body"), "{\"tab\":3}");
}

// =========================================================================
// 3. What a template costs
// =========================================================================

#[test]
fn a_template_free_program_pays_nothing_for_this_milestone() {
    // The whole argument for desugaring instead of adding a node: there is no
    // new runtime helper, so there is nothing to gate and nothing to pay for.
    // A template and the concatenation it means compile to the *same* module,
    // byte for byte -- which is the strongest form this claim can take.
    for (template, written_out) in [
        ("return `abc`;", "return \"abc\";"),
        ("return `a${1}b`;", "return \"a\" + 1 + \"b\";"),
        // The middle `""` is dropped like any other empty piece; only the
        // head's survives.
        ("return `${1}${2}`;", "return \"\" + 1 + 2;"),
        ("return `a${1}`;", "return \"a\" + 1;"),
        ("let x = 1; return `${x}`;", "let x = 1; return \"\" + x;"),
    ] {
        let a = compile_qjs_m1(template).expect("the template compiles");
        let b = compile_qjs_m1(written_out).expect("the concatenation compiles");
        assert_eq!(
            a, b,
            "{template:?} and {written_out:?} must be the same module"
        );
    }
}

#[test]
fn an_empty_piece_after_the_head_is_dropped_and_the_head_is_not() {
    // `` `a${b}` `` is `"a" + b` with no trailing `+ ""`, because once the
    // leftmost operand is a String the running value is a String forever.
    // The head's `""` is kept for the opposite reason -- it is what makes the
    // leftmost operand a String in the first place.
    assert_eq!(
        compile_qjs_m1("return `a${1}`;").unwrap(),
        compile_qjs_m1("return \"a\" + 1;").unwrap()
    );
    // Dropping the *head* would be a different program: `1 + "" + 2` is
    // "12" only by accident of the middle, and `1 + 2` would be 3.
    assert_ne!(
        compile_qjs_m1("return `${1}${2}`;").unwrap(),
        compile_qjs_m1("return 1 + 2;").unwrap(),
        "dropping the head would change what the program means"
    );
}

// =========================================================================
// 4. Malformed sources are named, not crashed
// =========================================================================

#[test]
fn an_unterminated_template_points_at_its_backtick() {
    let message = refuse("return `abc;");
    assert!(message.contains("close the template"), "{message}");
    assert!(message.contains("byte 7"), "{message}");
    // A template opened inside a substitution is the innermost thing open,
    // and the byte named is the outer template's -- which is the one the
    // author has to close for any of it to parse.
    let message = refuse("return `a${`b`;");
    assert!(
        message.contains("close the substitution in the template"),
        "{message}"
    );
    assert!(message.contains("byte 7"), "{message}");
}

#[test]
fn an_unterminated_substitution_is_named_too() {
    for source in ["return `a${1;", "return `a${;`", "return `${(1}`;"] {
        let message = refuse(source);
        assert!(
            message.starts_with("this engine "),
            "{source:?} does not speak for the engine: {message}"
        );
    }
}

#[test]
fn a_refused_escape_inside_a_template_is_named_by_the_escape() {
    // The piece is still consumed, so the refusal is about the escape and not
    // about a backtick that looked unterminated afterwards.
    let message = refuse(r#"return `\8`;"#);
    assert!(message.contains("octal"), "{message}");
}

// =========================================================================
// 5. What is still refused
// =========================================================================

#[test]
fn tagged_templates_are_refused_by_name() {
    // A tagged template is a *call* whose first argument is a frozen array of
    // the cooked strings carrying a `raw` property. This engine has neither
    // the array methods nor the property definition to build one, so it is
    // refused rather than approximated.
    refuses_capability(
        "function t(s) { return s; } return t`a`;",
        "tagged templates",
        Boundary::Subset,
    );
    refuses_capability(
        "function t(s) { return s; } return t`a${1}b`;",
        "tagged templates",
        Boundary::Subset,
    );
}

#[test]
fn the_neighbouring_constructs_are_still_refused_by_name() {
    // A template milestone is the one most likely to be mistaken for having
    // brought these; it did not. (Arrow functions were a row here and landed
    // right after templates did; `arrows_m3.rs` has them now.)
    refuses_capability(
        "class A {} return 1;",
        "the `class` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "return [1, , 2];",
        "elisions in an array literal",
        Boundary::FullJs,
    );
}
