//! The `Math` functions are gated per name like every method: a program
//! that names none carries none -- `Math.min()` and `Math.PI` fold to
//! literals and carry *nothing* -- and each named one has a published
//! price.

use tinyvm_qjs::compile_qjs_m1;

fn bytes(source: &str) -> usize {
    compile_qjs_m1(source)
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
        .len()
}

#[test]
fn a_program_that_names_no_math_function_pays_nothing() {
    let rows = [
        ("return 1;", 10_198),
        ("return 6 + 3;", 10_214),
        // The constants and the identity arities are literals: one byte
        // here is the literal's own encoding, not a prefab.
        ("return Math.PI;", 10_199),
        ("return Math.E;", 10_199),
        ("return Math.min();", 10_199),
        ("return Math.max();", 10_197),
        ("return Math.min(5);", 10_204),
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
fn each_function_has_a_published_price() {
    let base = bytes("return 6 + 3;");
    let rows = [
        ("return Math.floor(6.5) + 3;", 270),
        ("return Math.ceil(6.5) + 3;", 269),
        ("return Math.round(6.5) + 3;", 332),
        ("return Math.trunc(6.5) + 3;", 270),
        ("return Math.abs(-6.5) + 3;", 270),
        ("return Math.sqrt(6.5) + 3;", 269),
        ("return Math.sign(6.5) + 3;", 311),
        ("return Math.min(6, 3);", 277),
        ("return Math.max(6, 3);", 277),
        ("return Math.pow(6, 3);", 744),
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
