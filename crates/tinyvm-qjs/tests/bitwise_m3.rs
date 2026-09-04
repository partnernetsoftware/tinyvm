//! The bitwise operators are gated like a method: a program that writes
//! none carries none, and the ones that do pay for ToInt32 once and for
//! each operator they write. An operator has no name a call site could
//! ask for, so the gate is the scan's own -- exact, because the text
//! either contains `&` or it does not.

use tinyvm_qjs::compile_qjs_m1;

fn bytes(source: &str) -> usize {
    compile_qjs_m1(source)
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
        .len()
}

#[test]
fn a_program_that_writes_no_bitwise_operator_pays_nothing() {
    let rows = [
        ("return 1;", 10_198),
        ("return 6 + 3;", 10_214),
        ("let a = [1, 2]; return a.length;", 11_503),
        ("return \"ab\".includes(\"a\");", 11_014),
        // `&&` and `||` are not bitwise, and `!` is not `~`.
        ("return !(1 && 0 || 1);", 10_271),
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
fn each_operator_has_a_published_price() {
    let base = bytes("return 6 + 3;");
    let rows = [
        ("return 6 & 3;", 171),
        ("return 6 | 3;", 170),
        ("return 6 ^ 3;", 171),
        ("return 6 << 3;", 170),
        ("return 6 >> 3;", 170),
        ("return 6 >>> 3;", 171),
        ("return ~6 + 3;", 169),
        // ToInt32 is shared: the second operator costs only its body.
        ("return 6 & 3 | 3;", 219),
        ("let x = 6; x &= 3; return x;", 217),
    ];
    let got: Vec<usize> = rows
        .iter()
        .map(|(source, _)| bytes(source) - base)
        .collect();
    for ((source, _), n) in rows.iter().zip(&got) {
        println!("{source:?} costs {n} bytes over `return 6 + 3;`");
    }
    for ((source, want), n) in rows.iter().zip(&got) {
        assert_eq!(n, want, "{source:?} costs {n} bytes");
    }
}
