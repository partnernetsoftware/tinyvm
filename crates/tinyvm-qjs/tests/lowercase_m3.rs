//! `toLowerCase` -- ECMA-262 22.1.3.29, Unicode simple case mapping.
//!
//! The criteria of `plan/design-case-mapping-decision.md`, as tests. That
//! document priced four options and rejected three for lying somewhere; the
//! two tests that separate them are
//! [`caf_uppercase_lowercases_correctly`] (which an ASCII-only implementation
//! fails) and [`text_with_no_case_passes_through_untouched`] (which a
//! trap-on-non-ASCII implementation fails).
//!
//! Shipped alone rather than paired with `toUpperCase`, because all 67
//! downstream uses are `to_lower` and `to_upper` is at zero.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn text(source: &str) -> String {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()));
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    match Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}")) {
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            let bytes: &[u8] = &view;
            let at = ptr as usize;
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4")) as usize;
            String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8")
        }
        Value::Bool(b) => format!("{b}"),
        Value::Number(x) => format!("{x}"),
        other => panic!("{source:?}: unexpected {other:?}"),
    }
}

fn lower(input: &str) -> String {
    text(&format!("return \"{input}\".toLowerCase();"))
}

/// ASCII, which is what the corpus actually lowercases: platform names,
/// executable names, release tags, config lines.
#[test]
fn ascii_lowercases() {
    assert_eq!(lower("HELLO"), "hello");
    assert_eq!(lower("MiXeD CaSe 123!"), "mixed case 123!");
    assert_eq!(lower("already lower"), "already lower");
    assert_eq!(lower(""), "");
    assert_eq!(lower("Linux-AARCH64"), "linux-aarch64");
}

/// **Criterion ①.** The test that kills an ASCII-only implementation.
///
/// An implementation that maps only `A`-`Z` returns `"cafÉ"` here -- not an
/// approximation but a wrong answer, and one a comparison built on it would
/// branch wrongly from. 26 of the 1460 code points with a mapping are ASCII.
#[test]
fn caf_uppercase_lowercases_correctly() {
    assert_eq!(lower("CAFÉ"), "café");
    assert_eq!(lower("ÀÉÎÕÜ"), "àéîõü");
    // Not the final sigma -- see
    // `the_greek_final_sigma_is_the_second_named_divergence`.
    assert_eq!(lower("ΣΊΣΥΦΟΝ"), "σίσυφον");
    assert_eq!(lower("ПРИВЕТ"), "привет");
    assert_eq!(lower("ÇĞİÖŞÜ").contains('ç'), true);
}

/// **Criterion ②.** The test that kills "trap on anything non-ASCII".
///
/// Non-ASCII does not mean "has a case mapping": Chinese, Japanese, emoji and
/// already-lowercase Latin all have none, and refusing them would be a false
/// alarm on the overwhelming majority of non-ASCII text. Telling them apart
/// needs exactly the table that option was trying to avoid.
#[test]
fn text_with_no_case_passes_through_untouched() {
    assert_eq!(lower("中文没有大小写"), "中文没有大小写");
    assert_eq!(lower("日本語テスト"), "日本語テスト");
    assert_eq!(lower("😀🎉 emoji"), "😀🎉 emoji");
    assert_eq!(lower("café"), "café");
    assert_eq!(lower("→←↑↓ ¡¿"), "→←↑↓ ¡¿");
}

/// A mapping that makes the string **longer** in UTF-8.
///
/// U+023A and U+023E are two bytes and lowercase to three. They are the reason
/// the output buffer is `hl + hl/2` rather than `hl`, and they are the input
/// that would corrupt memory if it were `hl`.
#[test]
fn the_two_mappings_that_grow_the_string_are_handled() {
    assert_eq!(lower("\u{23a}"), "\u{2c65}");
    assert_eq!(lower("\u{23e}"), "\u{2c66}");
    assert_eq!(lower("\u{23a}\u{23e}\u{23a}"), "\u{2c65}\u{2c66}\u{2c65}");
    // Mixed with ASCII, so the copy has to keep both widths straight.
    assert_eq!(lower("A\u{23a}B"), "a\u{2c65}b");
}

/// A mapping that makes it **shorter**.
///
/// U+212A KELVIN SIGN is three bytes and lowercases to `k`, one byte. The
/// record's length is written from what was produced rather than from the
/// input, and this is the case that tells the difference.
#[test]
fn a_mapping_that_shrinks_the_string_writes_the_right_length() {
    assert_eq!(lower("\u{212a}"), "k");
    assert_eq!(lower("A\u{212a}B").len(), 3);
    assert_eq!(lower("A\u{212a}B"), "akb");
}

/// A four-byte character survives the round trip.
#[test]
fn four_byte_characters_survive() {
    assert_eq!(lower("𝕬BC"), "𝕬bc");
    assert_eq!(lower("😀A😀"), "😀a😀");
}

/// **Criterion ⑤, the named divergence.** `İ` does not lowercase.
///
/// U+0130 maps to two code points (`i` plus a combining dot), and this table
/// holds single-code-point mappings only. It passes through unchanged rather
/// than becoming a plain `i`, which would be a *different* wrong answer --
/// unchanged is at least honest about having done nothing.
#[test]
fn the_turkish_dotted_capital_i_is_a_named_divergence() {
    assert_eq!(
        lower("\u{130}"),
        "\u{130}",
        "U+0130 lowercases to two code points; this table holds one-to-one \
         mappings, so it is left alone rather than approximated"
    );
}

/// **A second named divergence, found by a test rather than by the design.**
///
/// ECMA-262 22.1.3.29 uses Unicode *full* case conversion, which includes the
/// `Final_Sigma` condition: a `Σ` at the end of a word lowercases to `ς`
/// (U+03C2), not `σ` (U+03C3). This table holds the *simple* mapping and
/// answers `σ` everywhere.
///
/// Deciding it needs a second table -- `Final_Sigma` is defined by what
/// surrounds the sigma, in terms of the `Cased` and `Case_Ignorable`
/// properties -- so it is not a line of code that was skipped, it is a second
/// data set the same size question applies to.
///
/// The decision document named `İ` as the only divergence and missed this one,
/// which is recorded there now: **a criteria list written from a table's shape
/// finds the mappings that are missing, not the ones that are conditional.**
///
/// It is narrow in the direction that matters for the corpus: both sides of a
/// case-insensitive comparison get the same answer, so comparing still works.
/// That is why it is recorded rather than treated as blocking.
#[test]
fn the_greek_final_sigma_is_the_second_named_divergence() {
    assert_eq!(
        lower("ΟΔΟΣ"),
        "οδοσ",
        "ECMA-262 gives `οδος`: a word-final sigma is ς. Deciding that needs \
         the Cased and Case_Ignorable properties, which is a second table"
    );
}

/// It composes with what the corpus writes: case-insensitive comparison.
#[test]
fn it_reads_the_way_the_corpus_uses_it() {
    let source = "const want = \"linux-x86_64\";
    const got = \"Linux-X86_64\";
    return got.toLowerCase() === want;";
    assert_eq!(text(source), "true");
}

/// **Criterion ③.** A program that never lowercases carries neither the table
/// nor the search.
///
/// The table is 8 076 bytes -- a third again of a bare module -- so this is
/// the gate that matters most in this milestone. The three sizes are the ones
/// `closures_m3.rs` has pinned since closures landed.
#[test]
fn a_program_that_never_lowercases_carries_none_of_it() {
    for (source, want) in [
        ("return 1;", 10_025),
        ("let o = {a:1}; o.b = 2; return o.a;", 10_193) /* +23 on 2026-08-29: a program that reads a static property can reach `__obj_get` with a String receiver, and the arm that names the missing property is 23 bytes; see runtime.rs `FAULT_MISSING_STRING_METHOD` */,
        (
            "function mk() { return function () { return 1; }; } let f = mk(); return f();",
            10_342,
        ),
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(n, want, "{source:?} is {n} bytes, so the table leaked into it");
    }
}

/// **Criterion ④.** What it costs, published rather than bounded.
///
/// An intercept and not a slope: a table is a one-time cost. The decision
/// document declines to set a ceiling on purpose -- whether the price is worth
/// paying is a product judgement, and this test's job is to make the number
/// impossible to lose rather than to approve it.
#[test]
fn what_lowercasing_costs_is_written_down() {
    let base = compile_qjs_m1("return \"AB\";").expect("compiles").len();
    let with = compile_qjs_m1("return \"AB\".toLowerCase();")
        .expect("compiles")
        .len();
    let cost = with - base;
    println!("toLowerCase: {cost} bytes, of which 8076 is the Unicode run table");
    assert!(
        cost > 8_076,
        "the table alone is 8076 bytes; {cost} is less than that, so something \
         is not being emitted"
    );
    assert!(
        cost < 12_000,
        "{cost} bytes is more than the table plus a plausible amount of code; \
         read why before accepting it"
    );
}
