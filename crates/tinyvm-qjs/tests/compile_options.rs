//! The seam between the language and the [`eval_wasm`] skin.
//!
//! One compiler serves two callers that genuinely disagree about what a bare
//! name means, and [`Options`] is where that disagreement lives. These tests
//! lock both halves of it, plus the narrowing that lets a rich diagnostic
//! cross into the fmt-free [`WasmError`].

use tinyvm::{HostGlobal, Val, WasmError, eval_wasm};
use tinyvm_qjs::{Boundary, Names, Options, compile_qjs, compile_qjs_with, qjs2wasm};

fn host_import(source: &str) -> Result<Vec<u8>, tinyvm_qjs::CompileError> {
    compile_qjs_with(
        source,
        Options {
            names: Names::HostImport,
        },
    )
}

#[test]
fn the_language_has_no_bindings_and_says_so() {
    for source in ["g", "g()", "g+2"] {
        let error = compile_qjs(source).expect_err("the default has nothing to resolve a name to");
        assert_eq!(
            error.message, "this engine does not support variable references yet",
            "{source:?}"
        );
        assert_eq!(error.boundary, Boundary::Subset, "{source:?}");
    }
}

#[test]
fn the_skin_resolves_a_name_to_a_zero_argument_host_import() {
    let g = [HostGlobal::new("js", "g", Val::I32(40))];
    for source in ["g", "g()"] {
        let wasm = host_import(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
        // Same bytes either way: `g` and `g()` are the same call, and the
        // skin's own entry point must agree with the option that spells it out.
        assert_eq!(qjs2wasm(source).ok().as_ref(), Some(&wasm), "{source:?}");
        match eval_wasm(&wasm, &g, &[]) {
            Ok(vals) if matches!(vals.as_slice(), [Val::I32(40)]) => {}
            Ok(_) => panic!("{source:?}: unexpected values"),
            Err(e) => panic!("{source:?}: {}", e.message()),
        }
    }
}

/// Every import entry here is `js` + a one-character field name, so this
/// two-byte prefix of the module name appears exactly once per import.
fn import_count(wasm: &[u8]) -> usize {
    wasm.windows(3).filter(|w| *w == b"\x02js").count()
}

#[test]
fn one_import_per_name_however_often_it_appears() {
    let two = [
        HostGlobal::new("js", "g", Val::I32(40)),
        HostGlobal::new("js", "h", Val::I32(2)),
    ];
    // Three mentions, two imports. A duplicate import entry would still run
    // and still return 82, so the value alone is not evidence -- the import
    // table has to be counted.
    let repeated = host_import("g+g+h").unwrap();
    assert_eq!(
        import_count(&repeated),
        2,
        "`g+g+h` must import g and h once"
    );
    assert_eq!(import_count(&host_import("g+h").unwrap()), 2);
    assert_eq!(import_count(&host_import("40+2").unwrap()), 0);
    match eval_wasm(&repeated, &two, &[]) {
        Ok(vals) if matches!(vals.as_slice(), [Val::I32(82)]) => {}
        Ok(_) => panic!("unexpected values"),
        Err(e) => panic!("{}", e.message()),
    }
}

/// The narrowing [`qjs2wasm`] performs is by declared category, never by
/// re-reading the sentence. Each boundary reaches the fmt-free face as its own
/// summary, so a reworded diagnostic cannot silently change what a caller sees.
#[test]
fn a_diagnostic_crosses_into_wasm_error_by_category() {
    for (source, boundary) in [
        ("const x = 1", Boundary::FullJs),
        ("eval(1)", Boundary::FullJs),
        ("g.x", Boundary::ThirdBinding),
        ("g(1)", Boundary::ThirdBinding),
        ("1.5", Boundary::Subset),
    ] {
        let rich = host_import(source).expect_err("outside the subset");
        assert_eq!(rich.boundary, boundary, "{source:?}: {rich}");
        // `WasmError` has no `Debug` -- the core is fmt-free -- so compare it
        // by hand rather than through `assert_eq!`.
        assert!(
            qjs2wasm(source) == Err(WasmError::Decode(boundary.terse())),
            "{source:?} did not narrow to its own boundary"
        );
        // The rich sentence is the thing worth keeping. It is a different
        // sentence from the summary, and it speaks for the engine.
        assert_ne!(rich.message, boundary.terse(), "{source:?}");
        assert!(
            rich.message.starts_with("this engine "),
            "{source:?} gave {:?}",
            rich.message
        );
    }
}

#[test]
fn every_boundary_has_a_distinct_summary() {
    let all = [Boundary::FullJs, Boundary::ThirdBinding, Boundary::Subset];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(
                a.terse(),
                b.terse(),
                "{a:?} and {b:?} are indistinguishable"
            );
        }
    }
}
