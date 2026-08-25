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
        // Every refusal that used to read as the guest's own fault. The two
        // `invoke` ones were `try_reserve_exact` failures on the host's own
        // argument and result vectors: an embedder out of memory was told the
        // guest had misbehaved, and retrying with a smaller guest would not
        // have helped.
        "invoke argument allocation",
        "invoke result allocation",
        "host argument allocation",
        "host result allocation",
        "call argument allocation",
        "function result allocation",
        "function type allocation",
        "locals allocation",
        "function reference allocation",
        "instance table allocation",
        "global state allocation",
        "data segment allocation",
        "element segment allocation",
        // And the one that read as a ceiling: a refused operand-stack growth
        // is not the VM's fixed bound being reached.
        "operand stack allocation",
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
    // ...and the allocator refusing to grow that same stack is a different
    // answer, because a different reaction is correct: wait for memory, do not
    // tell the embedder its own bound was hit.
    let refused = WasmError::Trap("operand stack allocation");
    assert!(refused.is_allocation());
    assert!(!refused.is_resource_ceiling());
    assert_ne!(operand.class(), refused.class());

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

/// The allocation *call sites*, checked from the call rather than from the
/// message.
///
/// [`every_fault_message_obeys_the_naming_rule_the_classifier_reads`] guards
/// the naming convention, but only for messages that already mention
/// allocation: it asks "does a message that says `alloc` end with
/// `allocation`". That question cannot be answered `no` by a refusal that
/// never says `alloc` at all. `Trap("invoke arguments")` and
/// `Trap("invoke results")` were both `try_reserve_exact` failures that
/// classified as `Guest` — an embedder under memory pressure was told its
/// guest was broken — and the naming guard passed them without a word,
/// because it was checking the convention against the things already keeping
/// it.
///
/// So this test starts from the allocator instead. It finds every
/// `try_reserve` / `try_reserve_exact` in the crate, follows the failure arm
/// to whatever it constructs, and requires that — whenever that is a
/// `WasmError` — `class()` reads it as [`WasmFaultClass::Allocation`]. A
/// refusal reported as any other error type is outside this classifier's
/// vocabulary and is listed, not judged; a refusal turned into a spec return
/// value (`memory.grow` and `table.grow` answer `-1`) is not a fault at all
/// and must construct none.
#[test]
fn every_allocation_call_site_produces_a_fault_the_classifier_calls_allocation() {
    let mut files = Vec::new();
    source_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() > 5, "expected the crate's source tree");
    let sources: Vec<(PathBuf, String)> = files
        .iter()
        .map(|path| {
            (
                path.clone(),
                fs::read_to_string(path).expect("read source file"),
            )
        })
        .collect();

    let mut faults = 0usize;
    let mut spec_values = 0usize;
    let mut other_error_types: Vec<String> = Vec::new();
    let mut misclassified: Vec<String> = Vec::new();
    let mut total = 0usize;

    for (path, source) in &sources {
        for start in allocation_calls(source) {
            total += 1;
            let site = format!("{}:{}", path.display(), line_of(source, start));
            match recovery(source, start) {
                Recovery::SpecValue => spec_values += 1,
                Recovery::Fault(expression) => {
                    match resolve_messages(&expression, &sources, source, start) {
                        Resolved::NotAWasmError(kind) => {
                            other_error_types.push(format!("{site}: {kind}"));
                        }
                        Resolved::Messages(messages) => {
                            assert!(
                                !messages.is_empty(),
                                "{site}: could not resolve any message from the failure arm \
                                 {expression:?}; the allocation message must stay statically \
                                 visible from the call site or this guard cannot check it"
                            );
                            for message in messages {
                                faults += 1;
                                let error =
                                    WasmError::Trap(Box::leak(message.clone().into_boxed_str()));
                                if error.class() != WasmFaultClass::Allocation {
                                    misclassified.push(format!(
                                        "{site}: a failed allocation constructs {message:?}, \
                                         which class() reads as {:?}, not Allocation",
                                        error.class()
                                    ));
                                }
                            }
                        }
                    }
                }
                Recovery::Unreadable => panic!(
                    "{site}: this guard could not find the failure arm of an allocation call. \
                     Write it as `.map_err(|_| ...)` or `.is_err()` so the refusal stays \
                     visible to the classifier guard."
                ),
            }
        }
    }

    // The scan must not quietly stop finding things: if a refactor renames the
    // allocation call or moves it behind a wrapper, these counts collapse and
    // the guard says so instead of passing an empty check.
    assert!(
        total > 80,
        "expected the crate's whole allocation surface, found {total} call sites"
    );
    assert!(
        faults > 50,
        "expected most allocation refusals to become faults, found {faults}"
    );
    assert!(
        spec_values > 0,
        "grow instructions answer an allocator refusal with -1, not a fault; found none"
    );

    assert!(
        misclassified.is_empty(),
        "an allocator refusal must classify as WasmFaultClass::Allocation so an embedder can \
         retry it instead of blaming the guest:\n{}",
        misclassified.join("\n")
    );

    // Not a failure: refusals that never reach `WasmError`. Listed so that a
    // new one is a visible decision rather than a silent gap.
    for line in &other_error_types {
        assert!(
            line.contains("WasiErrno")
                || line.contains("HostError")
                || line.contains("FfiError")
                || line.contains("PendingResultError")
                || line.contains("ResourceTableError")
                || line.contains(".to_string()")
                || line.contains("format!"),
            "unexpected non-WasmError allocation failure type: {line}"
        );
    }
}

/// Byte offsets of every `try_reserve` / `try_reserve_exact` call in `source`,
/// skipping commented-out lines.
///
/// These are the crate's only fallible allocation primitives; `alloc::alloc`
/// and `handle_alloc_error` appear nowhere, and infallible `Vec::reserve` /
/// `with_capacity` are what the crate is written to avoid.
fn allocation_calls(source: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(offset) = source[from..].find("try_reserve") {
        let start = from + offset;
        from = start + "try_reserve".len();
        let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
        if source[line_start..start].trim_start().starts_with("//") {
            continue;
        }
        // `try_reserve` and `try_reserve_exact` only; nothing else may sneak
        // past on a prefix match.
        let rest = &source[start..];
        if !(rest.starts_with("try_reserve(") || rest.starts_with("try_reserve_exact(")) {
            continue;
        }
        found.push(start);
    }
    found
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset].matches('\n').count() + 1
}

enum Recovery {
    /// `.map_err(|_| EXPR)` — the refusal becomes a fault built by `EXPR`.
    Fault(String),
    /// `.is_err()` guarding a spec-defined return value, constructing no fault.
    SpecValue,
    Unreadable,
}

/// The failure arm of the allocation call starting at `start`.
fn recovery(source: &str, start: usize) -> Recovery {
    let window = &source[start..source.len().min(start + 1600)];
    let map_err = window.find(".map_err(");
    let is_err = window.find(".is_err()");
    match (map_err, is_err) {
        (Some(m), rest) if rest.is_none_or(|i| m < i) => {
            let open = start + m + ".map_err".len();
            let argument = balanced(source, open);
            let body = match argument.find("| ") {
                // `|_| EXPR` / `|_error| EXPR`
                Some(_) => argument
                    .rsplit_once('|')
                    .map(|(_, tail)| tail)
                    .unwrap_or(&argument),
                None => &argument,
            };
            Recovery::Fault(normalise(body))
        }
        (_, Some(i)) => {
            // The only fault-free refusal the crate allows: `memory.grow` and
            // `table.grow` answer `-1`. Prove that is what this is by reading
            // the guarded block itself, not a fixed window that would run on
            // into whatever follows the `if`.
            let block = braced(source, start + i);
            if block.contains("Ok(") && !block.contains("WasmError") {
                Recovery::SpecValue
            } else {
                Recovery::Fault(normalise(&block))
            }
        }
        _ => Recovery::Unreadable,
    }
}

/// The text inside the parentheses opening at or after `open`.
fn balanced(source: &str, open: usize) -> String {
    let bytes = source.as_bytes();
    let mut index = open;
    while index < bytes.len() && bytes[index] != b'(' {
        index += 1;
    }
    let first = index + 1;
    let mut depth = 1usize;
    index = first;
    while index < bytes.len() && depth > 0 {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth > 0 {
            index += 1;
        }
    }
    source[first..index.min(source.len())].to_string()
}

/// The text inside the braces opening at or after `open`.
fn braced(source: &str, open: usize) -> String {
    let bytes = source.as_bytes();
    let mut index = open;
    while index < bytes.len() && bytes[index] != b'{' {
        index += 1;
    }
    let first = index + 1;
    let mut depth = 1usize;
    index = first;
    while index < bytes.len() && depth > 0 {
        match bytes[index] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        if depth > 0 {
            index += 1;
        }
    }
    source[first..index.min(source.len())].to_string()
}

fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

enum Resolved {
    Messages(Vec<String>),
    NotAWasmError(String),
}

/// Every message the failure arm can produce.
///
/// Three shapes reach a `WasmError`: a literal at the site, a `#[cold]`
/// helper the crate outlines to keep the static core small, and a
/// `&'static str` parameter threaded from the callers of the enclosing
/// function. All three are followed; anything else is reported as a different
/// error type rather than assumed harmless.
fn resolve_messages(
    expression: &str,
    sources: &[(PathBuf, String)],
    source: &str,
    start: usize,
) -> Resolved {
    let expression = expression.trim().trim_end_matches('?').trim();
    if !expression.contains("WasmError") {
        // A helper call: `table_allocation()`, `call_stack_allocation()`.
        if let Some(name) = expression.strip_suffix("()") {
            let name = name.trim();
            if is_identifier(name) {
                let messages = helper_messages(name, sources);
                if !messages.is_empty() {
                    return Resolved::Messages(messages);
                }
            }
        }
        return Resolved::NotAWasmError(expression.to_string());
    }

    let mut messages = Vec::new();
    for constructor in ["WasmError::Trap(", "WasmError::Decode("] {
        let mut from = 0usize;
        while let Some(offset) = expression[from..].find(constructor) {
            let open = from + offset + constructor.len() - 1;
            from = open + 1;
            let argument = balanced(expression, open);
            let argument = argument.trim();
            if let Some(literal) = argument.strip_prefix('"').and_then(|a| a.split('"').next()) {
                messages.push(literal.to_string());
            } else if is_identifier(argument) {
                // A `&'static str` parameter: check every literal the callers
                // thread through it.
                messages.extend(parameter_messages(argument, sources, source, start));
            }
        }
    }
    Resolved::Messages(messages)
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The messages a zero-argument fault helper returns.
fn helper_messages(name: &str, sources: &[(PathBuf, String)]) -> Vec<String> {
    let needle = format!("fn {name}(");
    let mut messages = Vec::new();
    for (_, source) in sources {
        let Some(index) = source.find(&needle) else {
            continue;
        };
        let body = &source[index..source.len().min(index + 400)];
        let body = body.split("\n}").next().unwrap_or(body);
        for constructor in ["WasmError::Trap(\"", "WasmError::Decode(\""] {
            for fragment in body.split(constructor).skip(1) {
                if let Some(message) = fragment.split('"').next() {
                    messages.push(message.to_string());
                }
            }
        }
    }
    messages
}

/// The literals passed for parameter `name` of the function enclosing `start`.
fn parameter_messages(
    name: &str,
    sources: &[(PathBuf, String)],
    source: &str,
    start: usize,
) -> Vec<String> {
    let head = &source[..start];
    let signature = head
        .rmatch_indices("fn ")
        .map(|(index, _)| index)
        .find(|index| {
            let before = head[..*index].rsplit('\n').next().unwrap_or("");
            before.trim().is_empty()
                || before.trim() == "pub"
                || before.trim().ends_with("pub")
                || before.trim().ends_with("unsafe")
        });
    let Some(signature) = signature else {
        return Vec::new();
    };
    let after = &source[signature + "fn ".len()..];
    let function = after
        .split(['(', '<'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let parameters = balanced(source, signature);
    let index = match split_arguments(&parameters)
        .iter()
        .position(|p| p.split(':').next().unwrap_or("").trim() == name)
    {
        Some(index) => index,
        None => return Vec::new(),
    };

    let mut messages = Vec::new();
    let needle = format!("{function}(");
    for (_, other) in sources {
        let mut from = 0usize;
        while let Some(offset) = other[from..].find(&needle) {
            let open = from + offset + needle.len() - 1;
            from = open + 1;
            // The definition itself is not a call.
            let before = other[..from - needle.len()].trim_end();
            if before.ends_with("fn") {
                continue;
            }
            let arguments = split_arguments(&balanced(other, open));
            if let Some(literal) = arguments.get(index).and_then(|argument| {
                argument
                    .trim()
                    .strip_prefix('"')
                    .and_then(|a| a.split('"').next())
            }) {
                messages.push(literal.to_string());
            }
        }
    }
    messages
}

/// Split an argument or parameter list at its top-level commas.
fn split_arguments(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for character in text.chars() {
        match character {
            '(' | '[' | '<' | '{' => depth += 1,
            ')' | ']' | '>' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(current.trim().to_string());
                current = String::new();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}
