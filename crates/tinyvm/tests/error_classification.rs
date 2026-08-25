//! Classifying a fault without matching its message text.
//!
//! The core is fmt-free, so a `WasmError` carries one `&'static str` and
//! nothing else. Downstream therefore classified faults by comparing that
//! string — and the moment the crate split its overloaded ceiling messages,
//! every such comparison silently stopped matching. Nothing failed; the
//! embedder just quietly stopped recognising its own budget exhaustion.
//!
//! These tests hold the replacement to the standard that makes it worth
//! having: the categories are derived from real executions wherever a guest
//! can reach them, no two ceilings answer the same, and the one naming
//! convention the classifier leans on is checked against the source rather
//! than assumed.

use std::fs;
use std::path::{Path, PathBuf};

use tinyvm::{Limits, Val, WasmCeiling, WasmError, WasmFaultClass, WasmModule};

/// Kept for the call sites that read better with a sentence than with a
/// `.expect`. It is no longer *needed*: [`WasmError`] derives `Debug`
/// unconditionally now, so `Result::expect` compiles here -- see
/// `fault_types_are_debug_printable_outside_this_crates_unit_tests`.
fn must_ok<T>(result: Result<T, WasmError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {}", error.message()),
    }
}

fn must_trap<T>(result: Result<T, WasmError>, context: &str) -> WasmError {
    match result {
        Ok(_value) => panic!("{context}: expected a fault"),
        Err(error) => error,
    }
}

fn wasm(source: &str) -> Vec<u8> {
    wat::parse_str(source).expect("assemble module")
}

/// Every ceiling the embedder configures, reached for real, then classified
/// without looking at what it said.
#[test]
fn every_host_configured_ceiling_names_its_own_limits_field() {
    // (module (func (export "spin") (loop br 0)))
    let spin = wasm(r#"(module (func (export "spin") (loop br 0)))"#);
    let steps = must_trap(
        must_ok(
            WasmModule::from_bytes_with(
                &spin,
                Limits {
                    max_steps: 64,
                    ..Limits::default()
                },
            ),
            "load spinner",
        )
        .instantiate()
        .and_then(|mut instance| instance.invoke_by_name("spin", &[])),
        "step budget",
    );

    // A guest that recurses until one of the two call-stack budgets stops it.
    let countdown = wasm(
        r#"(module (func $down (export "down") (param i32)
             (if (local.get 0)
               (then (call $down (i32.sub (local.get 0) (i32.const 1)))))))"#,
    );
    let call_depth = must_trap(
        must_ok(
            WasmModule::from_bytes_with(
                &countdown,
                Limits {
                    max_call_depth: 2,
                    ..Limits::default()
                },
            ),
            "load countdown",
        )
        .instantiate()
        .and_then(|mut instance| instance.invoke_by_name("down", &[Val::I32(16)])),
        "call depth",
    );
    let slots = must_trap(
        must_ok(
            WasmModule::from_bytes_with(
                &countdown,
                Limits {
                    max_activation_slots: 6,
                    ..Limits::default()
                },
            ),
            "load countdown",
        )
        .instantiate()
        .and_then(|mut instance| instance.invoke_by_name("down", &[Val::I32(16)])),
        "activation slots",
    );

    let pages = must_trap(
        WasmModule::from_bytes_with(
            &wasm("(module (memory 3))"),
            Limits {
                max_memory_pages: 2,
                ..Limits::default()
            },
        ),
        "memory pages",
    );
    let elems = must_trap(
        WasmModule::from_bytes_with(
            &wasm("(module (table 16 funcref))"),
            Limits {
                max_table_elems: 8,
                ..Limits::default()
            },
        ),
        "table elements",
    );

    let reached = [
        ("max_steps", steps, WasmCeiling::Steps),
        ("max_call_depth", call_depth, WasmCeiling::CallDepth),
        ("max_activation_slots", slots, WasmCeiling::ActivationSlots),
        ("max_memory_pages", pages, WasmCeiling::MemoryPages),
        ("max_table_elems", elems, WasmCeiling::TableElems),
    ];
    for (field, error, expected) in reached {
        assert!(
            error.ceiling() == Some(expected),
            "{field} reported {} but did not name its own ceiling",
            error.message()
        );
        assert!(
            error.is_resource_ceiling(),
            "{field} ({}) must classify as a resource ceiling",
            error.message()
        );
        assert!(error.class() == WasmFaultClass::ResourceCeiling);
        assert!(!error.is_internal(), "{field} is not an internal invariant");
        assert!(
            !error.is_allocation(),
            "{field} is not an allocator refusal"
        );
    }

    // Five budgets, five answers: an embedder can tell which number to raise.
    for (index, (left_field, left, _)) in reached.iter().enumerate() {
        for (right_field, right, _) in reached.iter().skip(index + 1) {
            assert!(
                left.ceiling() != right.ceiling(),
                "{left_field} and {right_field} cannot share a ceiling"
            );
        }
    }
}

/// Ordinary guest faults and load rejections must not read as budget
/// exhaustion — an embedder that retries with a bigger budget on those would
/// loop forever on a guest that is simply broken.
#[test]
fn guest_faults_and_load_rejections_are_never_ceilings() {
    let mut instance = must_ok(
        must_ok(
            WasmModule::from_bytes(&wasm(
                r#"(module
                     (memory 1)
                     (func (export "divide") (result i32)
                       (i32.div_s (i32.const 1) (i32.const 0)))
                     (func (export "wild") (result i32)
                       (i32.load (i32.const 1000000)))
                     (func (export "stop") unreachable))"#,
            )),
            "load faulty module",
        )
        .instantiate(),
        "instantiate faulty module",
    );

    for export in ["divide", "wild", "stop"] {
        let error = must_trap(instance.invoke_by_name(export, &[]), export);
        assert!(
            error.class() == WasmFaultClass::Guest,
            "{export} reported {} and must classify as a guest fault",
            error.message()
        );
        assert!(error.ceiling().is_none());
        assert!(!error.is_resource_ceiling());
        assert!(!error.is_internal());
    }

    // A module that never becomes invokable is a load rejection, whatever the
    // embedder's budgets are.
    let truncated = must_trap(WasmModule::from_bytes(b"\0asm\x01\0\0\0\x01"), "truncated");
    assert!(truncated.class() == WasmFaultClass::Load);
    assert!(!truncated.is_resource_ceiling());

    // Missing exports are the embedder's own mistake, not a ceiling.
    let absent = must_trap(instance.invoke_by_name("absent", &[]), "absent export");
    assert!(absent.class() == WasmFaultClass::Guest);
    assert!(absent.ceiling().is_none());
}

/// The conditions no guest input can reach, and the ones only the allocator
/// can cause, are separated from both ceilings and guest faults.
#[test]
fn allocation_refusals_and_internal_invariants_are_their_own_categories() {
    for message in [
        "activation slot overflow",
        "memory size overflow",
        "memory size accounting",
        "table size overflow",
    ] {
        let error = WasmError::Trap(message);
        assert!(
            error.is_internal(),
            "{message} must classify as an internal invariant"
        );
        assert!(!error.is_resource_ceiling());
        assert!(!error.is_allocation());
        assert!(error.ceiling().is_none());
    }

    for message in [
        "call stack allocation",
        "memory allocation",
        "table allocation",
        "control stack",
    ] {
        let error = WasmError::Trap(message);
        assert!(
            error.is_allocation(),
            "{message} must classify as an allocator refusal"
        );
        assert!(!error.is_resource_ceiling());
        assert!(!error.is_internal());
    }

    // The fixed operand-stack bound is a ceiling, but no `Limits` field
    // controls it, so it names none.
    let operand = WasmError::Trap("operand stack");
    assert!(operand.is_resource_ceiling());
    assert!(operand.ceiling().is_none());

    // A decode-time allocation refusal is an allocator refusal, not a load
    // rejection: the module may load fine once the host has memory again.
    assert!(WasmError::Decode("module allocation").is_allocation());
}

fn source_files(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            source_files(&path, found);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            found.push(path);
        }
    }
}

/// Every message literal in the crate, checked against the two rules the
/// classifier depends on.
///
/// `class()` recognises an allocator refusal by the message ending in
/// `allocation`. That is a naming rule, and a rule nothing enforces is a rule
/// that decays: the day someone writes `Trap("replay alloc failed")` the
/// classifier starts calling it a guest fault and no test notices. So the rule
/// is checked here against the source itself, not against a list that has to
/// be maintained alongside it.
#[test]
fn every_fault_message_obeys_the_naming_rule_the_classifier_reads() {
    let mut files = Vec::new();
    source_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() > 5, "expected the crate's source tree");

    let mut messages = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).expect("read source file");
        for (constructor, _) in [("WasmError::Trap(\"", 0), ("WasmError::Decode(\"", 0)] {
            for fragment in source.split(constructor).skip(1) {
                if let Some(message) = fragment.split('"').next() {
                    messages.push((file.clone(), message.to_string()));
                }
            }
        }
    }
    assert!(
        messages.len() > 200,
        "expected the crate's whole message vocabulary, found {}",
        messages.len()
    );

    for (file, message) in &messages {
        let file = file.display();
        // Rule one: `alloc` in a message means the allocator refused, and such
        // a message says so at its end where the classifier looks.
        if message.contains("alloc") {
            assert!(
                message.ends_with("allocation"),
                "{file}: {message:?} mentions allocation but does not end with `allocation`, \
                 so WasmError::class() will not recognise it"
            );
        }
        // `WasmError` holds `&'static str`; a test that reads messages out of
        // the source has to lend them that lifetime.
        let error = WasmError::Trap(Box::leak(message.clone().into_boxed_str()));
        if message.ends_with("allocation") {
            assert!(
                error.is_allocation(),
                "{file}: {message:?} must classify as an allocator refusal"
            );
        }
        // Rule two: no message classifies as a ceiling by accident. Only the
        // five documented ones may name a `Limits` field.
        if let Some(ceiling) = error.ceiling() {
            assert!(
                matches!(
                    ceiling,
                    WasmCeiling::Steps
                        | WasmCeiling::CallDepth
                        | WasmCeiling::ActivationSlots
                        | WasmCeiling::MemoryPages
                        | WasmCeiling::TableElems
                ),
                "{file}: {message:?} names an unexpected ceiling"
            );
        }
    }

    // Rule three: a message is a whole phrase. In a fmt-free core a literal
    // that stops where an interpolated value was meant to go can never be
    // completed at run time, so it reaches the reader as a sentence with its
    // last word missing -- `Trap("memory access [")` and
    // `Trap("expected i32 on stack, got")` both shipped that way. The
    // punctuation and the trailing verbs below are what those looked like.
    for (file, message) in &messages {
        let file = file.display();
        for dangling in [",", ":", ";", "[", "(", "`", "-", "="] {
            assert!(
                !message.ends_with(dangling),
                "{file}: {message:?} ends on {dangling:?}, where a value it cannot \
                 interpolate was meant to go"
            );
        }
        for verb in ["got", "expected", "found", "was", "is", "than"] {
            assert!(
                message != verb && !message.ends_with(&format!(" {verb}")),
                "{file}: {message:?} ends on {verb:?} and never says what"
            );
        }
    }

    // The overloaded words that were split are gone for good: nothing may go
    // back to reporting a bare `call stack`, `memory size` or `table size`,
    // each of which used to mean three or four different things.
    for retired in ["call stack", "memory size", "table size"] {
        assert!(
            !messages.iter().any(|(_, message)| message == retired),
            "the overloaded message {retired:?} must not come back"
        );
    }
}

/// `Debug` has to be reachable from *here*, not only from the crate's own unit
/// tests.
///
/// `#[cfg_attr(test, derive(Debug))]` sets `cfg(test)` for `crates/tinyvm/src`
/// and nowhere else, so an integration test -- and every downstream crate --
/// saw these types as un-printable and un-`unwrap`able. The cost was a
/// hand-rolled `must_ok` in each test file, panicking through `message()` and
/// throwing away everything `assert_eq!` would have shown. The core stays
/// fmt-free because nothing in it formats a fault, which is what
/// `measure-core.sh` measures; naming the trait costs the static core nothing.
#[test]
fn fault_types_are_debug_printable_outside_this_crates_unit_tests() {
    // The three fault vocabularies, printed.
    assert_eq!(
        format!("{:?}", WasmError::Trap("call depth")),
        r#"Trap("call depth")"#
    );
    assert_eq!(
        format!("{:?}", WasmError::Decode("module allocation")),
        r#"Decode("module allocation")"#
    );
    assert_eq!(format!("{:?}", WasmCeiling::MemoryPages), "MemoryPages");
    assert_eq!(
        format!("{:?}", WasmFaultClass::ResourceCeiling),
        "ResourceCeiling"
    );
    assert_eq!(format!("{:?}", Val::I32(-1)), "I32(-1)");

    // Which is what makes these compile at all: `expect`, `unwrap_err` and
    // `assert_eq!` on a fault all need `Debug`, and none of them compiled from
    // an integration test before.
    let module = WasmModule::from_bytes(&wasm(r#"(module (func (export "stop") unreachable))"#))
        .expect("load a module without a hand-rolled helper");
    let error = module
        .instantiate()
        .expect("instantiate")
        .invoke_by_name("stop", &[])
        .expect_err("unreachable must trap");
    assert_eq!(error, WasmError::Trap("unreachable executed"));
    assert_eq!(error.class(), WasmFaultClass::Guest);
    assert_eq!(error.ceiling(), None);
}
