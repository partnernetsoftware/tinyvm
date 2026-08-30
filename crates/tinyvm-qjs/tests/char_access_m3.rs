//! Character access is gated like every method -- and `s[i]`, which is not
//! a name, is gated on the one thing the text can see: a computed read
//! whose key it does not settle as a String. `o["a"]` and the `for … of`
//! fold's own index read turn nothing on; `o[k]`, `a[i]` and `s[0]` do,
//! and pay for `__m_str_index` and the code-unit walk behind it. Not
//! exact, and it cannot be: what a receiver holds is a run-time fact.

use tinyvm_qjs::compile_qjs_m1;

fn bytes(source: &str) -> usize {
    compile_qjs_m1(source)
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
        .len()
}

#[test]
fn a_program_that_never_indexes_a_string_pays_nothing() {
    let rows = [
        ("return 1;", 10_198),
        // A String-literal key is settled by the text.
        ("let o = {a:1}; return o[\"a\"];", 10_570),
        // The `for … of` fold's index read is its own and turns nothing on.
        (
            "let n = 0; for (const x of [1, 2]) { n = n + x; } return n;",
            12_378,
        ),
        // The other String methods are behind their own names.
        ("return \"abc\".slice(1, 2);", 11_540),
    ];
    let got: Vec<usize> = rows.iter().map(|(source, _)| bytes(source)).collect();
    for ((source, _), n) in rows.iter().zip(&got) {
        println!("{source:?} is {n} bytes");
    }
    for ((source, want), n) in rows.iter().zip(&got) {
        assert_eq!(n, want, "{source:?} is {n} bytes");
    }
}

#[test]
fn each_access_has_a_published_price() {
    let base = bytes("return \"abc\".length;");
    let rows = [
        ("return \"abc\".charCodeAt(1);", 733),
        ("return \"abc\".charAt(1);", 869),
        ("return \"abc\".substring(1, 2);", 1_026),
        ("return \"abc\".substring(1);", 909),
        ("return \"abc\".slice(1, 2);", 1_129),
        // `s[i]` in a program with no arrays: the emitter's road.
        ("let s = \"abc\"; let i = 1; return s[i];", 768),
        // A computed read that is not on a String pays the same, because
        // the text cannot tell: this row is the price of the gate's
        // inexactness, and the reason it is written down.
        ("let o = {a:1}; let k = \"a\"; return o[k];", 1_101),
    ];
    let got: Vec<usize> = rows
        .iter()
        .map(|(source, _)| bytes(source) - base)
        .collect();
    for ((source, _), n) in rows.iter().zip(&got) {
        println!("{source:?} costs {n} bytes over a length-only program");
    }
    for ((source, want), n) in rows.iter().zip(&got) {
        assert_eq!(n, want, "{source:?} costs {n} bytes");
    }
}
