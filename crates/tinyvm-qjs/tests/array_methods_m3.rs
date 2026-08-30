//! The 2026-08-31 array methods are gated like every other: a program that
//! never names one pays nothing, and the ones that do pay a published price.
//!
//! `indexOf` and `includes` are the exception worth stating: the name is
//! shared with the String method and the text cannot tell the receivers
//! apart, so one prefab carries both arms and a program that calls either
//! on a String now carries the Array arm too. The row for `"ab".includes`
//! below is that price.

use tinyvm_qjs::compile_qjs_m1;

fn bytes(source: &str) -> usize {
    compile_qjs_m1(source)
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
        .len()
}

#[test]
fn a_program_that_never_names_them_pays_nothing() {
    let rows = [
        ("return 1;", 10_198),
        // The array set alone; no method.
        ("let a = [1, 2]; return a.length;", 11_503),
        // `push` and `pop`, which are not in this batch.
        ("let a = [1]; a.push(2); return a.pop();", 11_833),
        // The String `includes`, which now carries the Array arm: +221 on
        // 2026-08-31 (10 799 -> 11 020). See the module comment.
        ("return \"ab\".includes(\"a\");", 11_020),
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
fn each_method_has_a_published_price() {
    let base = bytes("let a = [1, 2]; return a.length;");
    let rows = [
        ("let a = [1, 2]; return Array.isArray(a);", 59),
        ("let a = [1, 2]; return a.indexOf(2);", 702),
        ("let a = [1, 2]; return a.includes(2);", 623),
        ("let a = [1, 2]; return a.concat(3).length;", 569),
        ("let a = [1, 2]; return a.concat(3, 4).length;", 639),
        ("let a = [1, 2]; return a.join(\"-\").length;", 871),
        ("let a = [1, 2]; return a.join().length;", 885),
    ];
    let got: Vec<usize> = rows
        .iter()
        .map(|(source, _)| bytes(source) - base)
        .collect();
    for ((source, _), n) in rows.iter().zip(&got) {
        println!("{source:?} costs {n} bytes over the array set");
    }
    for ((source, want), n) in rows.iter().zip(&got) {
        assert_eq!(n, want, "{source:?} costs {n} bytes over the array set");
    }
}
