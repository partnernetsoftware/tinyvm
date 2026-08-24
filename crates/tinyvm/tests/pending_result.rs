//! The two-pass variable-length return, as a mechanism.
//!
//! A host callback holds `&mut` on guest memory for its whole body, so it can
//! never re-enter the guest to call an exported allocator. Every embedder that
//! must hand a variable-length result back therefore improvises the same
//! protocol: call, get a length, allocate, ask for the bytes. These tests pin
//! the mechanism's contract — bounded, fallible, delivered at most once, and a
//! too-small destination that loses nothing — and then drive it through a real
//! guest so the protocol is proven to close, not just to compile.

use core::cell::RefCell;
use std::rc::Rc;

use tinyvm::{PendingResult, PendingResultError, Val, WasmError, WasmModule, guest_str};

fn must_ok<T>(result: Result<T, WasmError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {}", error.message()),
    }
}

#[test]
fn a_staged_result_is_bounded_delivered_once_and_never_partially_copied() {
    let mut pending = PendingResult::new(8);
    assert!(!pending.is_staged());
    assert_eq!(pending.len(), 0);
    assert!(pending.is_empty());
    assert_eq!(pending.as_slice(), b"");
    assert_eq!(pending.max_bytes(), 8);

    // Nothing staged: pass two on its own is a distinct, reportable condition,
    // not an empty success that the guest would read as a zero-length result.
    let mut destination = [0u8; 8];
    assert_eq!(
        pending.copy_out(&mut destination),
        Err(PendingResultError::NotStaged)
    );

    // Pass one. The length the guest is told is the length it later gets.
    assert_eq!(pending.stage(b"abcde"), Ok(5));
    assert!(pending.is_staged());
    assert_eq!(pending.len(), 5);
    assert_eq!(pending.as_slice(), b"abcde");

    // A second stage would drop a result the guest is still entitled to.
    assert_eq!(pending.stage(b"xy"), Err(PendingResultError::AlreadyStaged));
    assert_eq!(pending.as_slice(), b"abcde", "the first result survives");

    // The ceiling is enforced against the guest-chosen size, and enforced
    // before any allocation is attempted.
    let mut bounded = PendingResult::new(4);
    assert_eq!(bounded.stage(b"abcde"), Err(PendingResultError::OverBudget));
    assert!(!bounded.is_staged());
    assert_eq!(bounded.stage(b"abcd"), Ok(4), "exactly at the ceiling fits");

    // Pass two with a destination one byte short: nothing is written, the
    // staged bytes stay collectable, and the report says how much was needed.
    let mut small = [0xAAu8; 4];
    assert_eq!(
        pending.copy_out(&mut small),
        Err(PendingResultError::DestinationTooSmall { needed: 5 })
    );
    assert_eq!(&small, b"\xaa\xaa\xaa\xaa", "a refused copy writes nothing");
    assert!(pending.is_staged(), "a refused copy loses nothing");

    // Pass two, sized right. A longer destination keeps its tail.
    let mut destination = [0xBBu8; 8];
    assert_eq!(pending.copy_out(&mut destination), Ok(5));
    assert_eq!(&destination, b"abcde\xbb\xbb\xbb");

    // Delivered once: the same result cannot be collected twice.
    assert!(!pending.is_staged());
    assert_eq!(
        pending.copy_out(&mut destination),
        Err(PendingResultError::NotStaged)
    );

    // A staged empty result is a result, not "nothing staged" — the caller
    // tells them apart with `is_staged`.
    assert_eq!(pending.stage(b""), Ok(0));
    assert!(pending.is_staged());
    assert!(pending.is_empty());
    assert_eq!(pending.copy_out(&mut []), Ok(0));
    assert!(!pending.is_staged());

    // `restage` is the opt-in for a protocol where a new request abandons the
    // previous, uncollected result.
    assert_eq!(pending.stage(b"old"), Ok(3));
    assert_eq!(pending.restage(b"new"), Ok(3));
    assert_eq!(pending.as_slice(), b"new");

    // `clear` drops an abandoned result so the next request cannot collect it.
    pending.clear();
    assert!(!pending.is_staged());
    assert_eq!(
        pending.copy_out(&mut destination),
        Err(PendingResultError::NotStaged)
    );
}

#[test]
fn copying_out_to_a_guest_window_is_bounds_checked_and_loses_nothing() {
    let mut pending = PendingResult::new(64);
    let mut memory = [0u8; 8];

    // Nothing staged is reported before the window is even resolved.
    assert_eq!(
        pending
            .copy_out_to_guest(&mut memory, 0, 8)
            .map_err(|error| error.message()),
        Err("pending result missing")
    );

    assert_eq!(pending.stage(b"reply"), Ok(5));

    // An out-of-range or overflowing guest window is refused with the same
    // pointer fault the memory accessors raise, and leaves the result staged.
    for (ptr, len) in [(8i32, 8i32), (-1, 8), (0, -1), (4, 8)] {
        assert_eq!(
            pending
                .copy_out_to_guest(&mut memory, ptr, len)
                .map_err(|error| error.message()),
            Err("guest memory window"),
            "({ptr}, {len}) must be refused"
        );
    }
    assert!(pending.is_staged());
    assert_eq!(memory, [0u8; 8], "a refused copy never touches memory");

    // An in-range window that is too small is the protocol's own condition,
    // distinct from a bad pointer, and equally lossless.
    assert_eq!(
        pending
            .copy_out_to_guest(&mut memory, 0, 4)
            .map_err(|error| error.message()),
        Err("pending result destination")
    );
    assert!(pending.is_staged());
    assert_eq!(memory, [0u8; 8]);

    // Delivered into the guest's own bytes, at the offset the guest chose.
    assert_eq!(
        must_ok(
            pending.copy_out_to_guest(&mut memory, 3, 5),
            "copy out to guest"
        ),
        5
    );
    assert_eq!(&memory, b"\0\0\0reply");
    assert!(!pending.is_staged());
}

/// The protocol, closed end to end through a real guest.
///
/// The guest asks the host to reverse a string it owns. The host cannot hand
/// back the bytes — it holds `&mut` on the guest's memory and cannot call the
/// guest's allocator — so it stages them and returns the length. The guest then
/// reserves that many bytes out of its own static arena and asks for the copy.
/// Nothing here is named by tinyvm: `reverse`, `collect` and the arena are this
/// test's ABI, which is the point.
#[test]
fn a_guest_collects_a_variable_length_host_result_in_two_passes() {
    fn run(request: &str, guest_capacity: i32) -> Result<Vec<Val>, WasmError> {
        let wasm = wat::parse_str(
            r#"(module
                 (import "env" "reverse" (func $reverse (param i32 i32) (result i32)))
                 (import "env" "collect" (func $collect (param i32 i32) (result i32)))
                 (memory (export "memory") 1)
                 (global $arena (mut i32) (i32.const 256))
                 ;; (request_ptr, request_len, capacity) -> collected length
                 (func (export "round_trip") (param i32 i32 i32) (result i32)
                   (local $needed i32)
                   ;; pass one: the host stages the result and returns its size
                   (local.set $needed (call $reverse (local.get 0) (local.get 1)))
                   ;; the guest allocates out of its own arena, which only the
                   ;; guest can do, and only outside the host callback
                   (if (i32.gt_u (local.get $needed) (local.get 2))
                     (then (return (i32.const -1))))
                   ;; pass two: ask the host to fill the buffer just allocated
                   (call $collect (global.get $arena) (local.get 2))))"#,
        )
        .expect("assemble two-pass module");
        let mut module = must_ok(WasmModule::from_bytes(&wasm), "load two-pass module");

        // One buffer, shared by the two host imports. This is the whole of what
        // tinyvm provides; the two imports are the embedder's own ABI.
        let pending = Rc::new(RefCell::new(PendingResult::new(1024)));

        let staging = Rc::clone(&pending);
        let binding = module.bind_import_typed("env", "reverse", move |args, memory| {
            let (ptr, len) = match args {
                [Val::I32(ptr), Val::I32(len)] => (*ptr, *len),
                _ => return Err(WasmError::Trap("host argument type")),
            };
            let text = guest_str(memory, ptr, len)?;
            // The result's size is only known now, while guest memory is
            // borrowed and the guest's allocator is out of reach.
            let reversed: Vec<u8> = text.bytes().rev().collect();
            let needed = staging.borrow_mut().stage(&reversed)?;
            Ok(vec![Val::I32(needed as i32)])
        });
        must_ok(binding, "bind env.reverse");

        let collecting = Rc::clone(&pending);
        let binding = module.bind_import_typed("env", "collect", move |args, memory| {
            let (ptr, len) = match args {
                [Val::I32(ptr), Val::I32(len)] => (*ptr, *len),
                _ => return Err(WasmError::Trap("host argument type")),
            };
            let written = collecting
                .borrow_mut()
                .copy_out_to_guest(memory, ptr, len)?;
            Ok(vec![Val::I32(written as i32)])
        });
        must_ok(binding, "bind env.collect");

        let mut instance = must_ok(module.instantiate(), "instantiate two-pass module");
        {
            let mut view = instance.memory_mut()?;
            view[..request.len()].copy_from_slice(request.as_bytes());
        }
        let result = instance.invoke_by_name(
            "round_trip",
            &[
                Val::I32(0),
                Val::I32(request.len() as i32),
                Val::I32(guest_capacity),
            ],
        )?;
        if let [Val::I32(written)] = result.as_slice()
            && *written > 0
        {
            let written = *written;
            let view = instance.memory()?;
            let collected = &view[256..256 + written as usize];
            assert_eq!(collected, request.bytes().rev().collect::<Vec<u8>>());
        }
        Ok(result)
    }

    // A result the host sizes at run time crosses the boundary whole.
    assert!(matches!(
        run("tinyvm two-pass", 64).as_deref(),
        Ok([Val::I32(15)])
    ));
    // A different length through the same code path: the length really is the
    // host's to choose.
    assert!(matches!(run("ab", 64).as_deref(), Ok([Val::I32(2)])));
    // An empty result still completes both passes.
    assert!(matches!(run("", 64).as_deref(), Ok([Val::I32(0)])));

    // The guest's own capacity check, using the length pass one gave it: the
    // guest declines rather than asking for a copy that could not fit.
    assert!(matches!(
        run("too long for this", 4).as_deref(),
        Ok([Val::I32(-1)])
    ));
}
