//! The sanctioned `(ptr, len)` door into guest linear memory.
//!
//! Every embedder that binds a host import has to turn two guest-chosen `i32`
//! values into a slice, and that is the one arithmetic in an embedding whose
//! failure reads or writes host memory. These tests exhaust the boundary rather
//! than sampling it: for three memory sizes they enumerate *every* `(ptr, len)`
//! pair in and around the memory and check the accessor against an
//! independently computed expectation, then add the adversarial values an
//! exhaustive small sweep can never reach — `i32::MIN`, `-1`, `i32::MAX` — on
//! their own.

use tinyvm::{
    Val, WasmError, WasmModule, guest_bytes, guest_bytes_mut, guest_str, guest_window, guest_write,
};

const MEMORY: [u8; 8] = *b"abcdefgh";

/// The one truth the accessors must agree with, computed independently of them
/// in 64-bit arithmetic that cannot itself overflow: a window is admissible
/// exactly when its unsigned start plus its unsigned length lands at or before
/// the end of the memory.
fn admissible(memory_len: usize, ptr: i32, len: i32) -> Option<(usize, usize)> {
    let start = u64::from(ptr as u32);
    let end = start + u64::from(len as u32);
    if end > memory_len as u64 {
        return None;
    }
    Some((start as usize, end as usize))
}

fn refused<T>(result: Result<T, WasmError>) -> &'static str {
    match result {
        Ok(_value) => panic!("expected the window to be refused"),
        Err(error) => error.message(),
    }
}

fn message<T>(result: Result<T, WasmError>) -> Result<T, &'static str> {
    result.map_err(|error| error.message())
}

/// `WasmError` carries no `Debug` outside the crate's own unit tests — the core
/// is fmt-free — so integration tests unwrap through the static message.
fn must_ok<T>(result: Result<T, WasmError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {}", error.message()),
    }
}

#[test]
fn guest_windows_are_bounds_checked_over_the_whole_boundary() {
    // Exhaustive over small memories: 0..=12 covers every in-range address,
    // the address one past the end, and addresses well beyond it, crossed with
    // every length in the same span. 507 pairs, no sampling.
    for memory_len in [0usize, 1, MEMORY.len()] {
        let memory = &MEMORY[..memory_len];
        for ptr in 0..=12i32 {
            for len in 0..=12i32 {
                match admissible(memory_len, ptr, len) {
                    Some((start, end)) => {
                        let window = guest_window(memory_len, ptr, len)
                            .unwrap_or_else(|_| panic!("{ptr}+{len} of {memory_len} is in range"));
                        assert_eq!(window, start..end, "window {ptr}+{len} of {memory_len}");
                        assert_eq!(
                            guest_bytes(memory, ptr, len).ok(),
                            Some(&memory[start..end]),
                            "bytes {ptr}+{len} of {memory_len}"
                        );
                    }
                    None => {
                        assert_eq!(
                            refused(guest_window(memory_len, ptr, len)),
                            "guest memory window",
                            "window {ptr}+{len} of {memory_len} must be refused"
                        );
                        assert_eq!(
                            refused(guest_bytes(memory, ptr, len)),
                            "guest memory window",
                            "bytes {ptr}+{len} of {memory_len} must be refused"
                        );
                    }
                }
            }
        }
    }

    // The cases the sweep covers but which deserve to be stated: a zero-length
    // window at the very end is legal, the last byte is legal, one byte past
    // the end is not.
    assert_eq!(guest_bytes(&MEMORY, 8, 0).ok(), Some(&b""[..]));
    assert_eq!(guest_bytes(&MEMORY, 7, 1).ok(), Some(&b"h"[..]));
    assert_eq!(refused(guest_bytes(&MEMORY, 8, 1)), "guest memory window");
    // A zero-length window *past* the end is still a bad pointer, exactly as a
    // zero-length bulk-memory operation past the end is a trap.
    assert_eq!(refused(guest_bytes(&MEMORY, 9, 0)), "guest memory window");

    // An empty memory admits only the empty window at zero.
    assert_eq!(guest_bytes(&[], 0, 0).ok(), Some(&b""[..]));
    assert_eq!(refused(guest_bytes(&[], 0, 1)), "guest memory window");
    assert_eq!(refused(guest_bytes(&[], 1, 0)), "guest memory window");
}

#[test]
fn negative_and_overflowing_guest_windows_are_refused() {
    // Guest `i32` addresses are unsigned. `-1` is 4294967295, not a small
    // negative offset that could wrap backwards out of the memory.
    for (ptr, len) in [
        (-1i32, 0i32),
        (-1, 1),
        (i32::MIN, 0),
        (i32::MIN, 1),
        (0, -1),
        (1, -1),
        (0, i32::MIN),
        (-1, -1),
        (i32::MAX, i32::MAX),
        (i32::MIN, i32::MIN),
        (i32::MAX, 1),
        (1, i32::MAX),
    ] {
        assert_eq!(
            refused(guest_bytes(&MEMORY, ptr, len)),
            "guest memory window",
            "({ptr}, {len}) must be refused"
        );
        assert_eq!(
            refused(guest_write(&mut [0u8; 8], ptr, len, b"")),
            "guest memory window",
            "writing ({ptr}, {len}) must be refused"
        );
    }

    // Reinterpreted, this pair is 4294967295 + 2, which wraps to 1 in 32-bit
    // arithmetic — an address that *is* inside this memory. The sum must be
    // computed wide enough that it cannot fold back into range.
    assert_eq!(refused(guest_bytes(&MEMORY, -1, 2)), "guest memory window");

    // The same pair against a memory as large as Wasm allows one to be. Four
    // gibibytes is 65536 pages, the standard maximum, and the window still ends
    // one byte outside it.
    if let Ok(four_gib) = usize::try_from(1u64 << 32) {
        assert_eq!(
            refused(guest_window(four_gib, -1, 2)),
            "guest memory window"
        );
        // ...while the last byte of that same memory remains reachable, so the
        // rejection above is arithmetic, not a blanket ban on high addresses.
        assert_eq!(
            guest_window(four_gib, -1, 1).ok(),
            Some(0xFFFF_FFFF..0x1_0000_0000)
        );
    }
}

#[test]
fn guest_text_separates_a_bad_pointer_from_bad_utf8() {
    let mut memory = [0u8; 8];
    memory[..5].copy_from_slice(b"hello");
    assert_eq!(message(guest_str(&memory, 0, 5)), Ok("hello"));
    assert_eq!(message(guest_str(&memory, 2, 3)), Ok("llo"));
    assert_eq!(message(guest_str(&memory, 8, 0)), Ok(""));

    // Multi-byte UTF-8 is accepted whole and refused when split.
    let mut utf8 = [0u8; 8];
    utf8[..5].copy_from_slice("é€".as_bytes());
    assert_eq!(message(guest_str(&utf8, 0, 5)), Ok("é€"));
    assert_eq!(message(guest_str(&utf8, 0, 3)), Err("guest memory utf-8"));
    assert_eq!(message(guest_str(&utf8, 1, 4)), Err("guest memory utf-8"));

    // A lone continuation byte is bad text; an address past the end is a bad
    // pointer. An embedder that must tell "the guest sent garbage" from "the
    // guest sent a wild pointer" gets two different messages for them.
    assert_eq!(message(guest_str(&[0x80], 0, 1)), Err("guest memory utf-8"));
    assert_eq!(refused(guest_str(&memory, 0, 9)), "guest memory window");
    // An in-range window is checked for text only after it is checked for
    // range, so a wild pointer never reports as bad text.
    assert_eq!(refused(guest_str(&[0x80], 0, 2)), "guest memory window");
}

#[test]
fn guest_writes_never_partially_fill_an_undersized_window() {
    let mut memory = [0u8; 8];
    assert_eq!(message(guest_write(&mut memory, 2, 4, b"abc")), Ok(3));
    assert_eq!(
        &memory, b"\0\0abc\0\0\0",
        "only the source bytes are written"
    );

    // Exactly filling a window that ends at the last byte of the memory.
    assert_eq!(message(guest_write(&mut memory, 4, 4, b"wxyz")), Ok(4));
    assert_eq!(&memory, b"\0\0abwxyz");

    // Zero bytes into a zero-length window at the end of memory is a no-op,
    // not a fault.
    assert_eq!(message(guest_write(&mut memory, 8, 0, b"")), Ok(0));

    // One byte too many is refused whole, and reports its own condition: the
    // pointer was fine, the guest's buffer was not.
    let before = memory;
    assert_eq!(
        message(guest_write(&mut memory, 0, 3, b"abcd")),
        Err("guest memory window too small")
    );
    assert_eq!(memory, before, "a refused write must not fill the window");

    // An out-of-range window is refused before the source length is consulted,
    // and reports the pointer fault rather than the size fault.
    assert_eq!(
        refused(guest_write(&mut memory, 6, 4, b"")),
        "guest memory window"
    );
    assert_eq!(
        refused(guest_write(&mut memory, -1, 1, b"z")),
        "guest memory window"
    );
    assert_eq!(memory, before);

    // The mutable borrow door checks the same boundary as the read door.
    must_ok(
        guest_bytes_mut(&mut memory, 0, 2),
        "in-range mutable window",
    )
    .fill(0xEE);
    assert_eq!(&memory[..2], b"\xee\xee");
    assert_eq!(
        refused(guest_bytes_mut(&mut memory, 0, 9)),
        "guest memory window"
    );
    assert_eq!(
        refused(guest_bytes_mut(&mut memory, 9, 0)),
        "guest memory window"
    );
}

/// The accessors exist to be called from inside a `bind_import_typed`
/// callback, which holds the selected memory as a bare `&mut [u8]`. This is
/// that call, end to end: read a guest string out of linear memory, write a
/// reply back into the buffer the guest offered, and let a refused window
/// become the guest's trap instead of a host panic or a silent truncation.
#[test]
fn guest_memory_accessors_work_inside_a_host_callback() {
    fn run(reply_capacity: i32) -> Result<Vec<Val>, WasmError> {
        let wasm = wat::parse_str(
            r#"(module
                 (import "env" "shout" (func $shout (param i32 i32 i32 i32) (result i32)))
                 (memory 1)
                 (data (i32.const 0) "tinyvm")
                 (func (export "run") (result i32)
                   i32.const 0 i32.const 6 i32.const 16 i32.const 8 call $shout))"#,
        )
        .expect("assemble shout module");
        let mut module = must_ok(WasmModule::from_bytes(&wasm), "load shout module");
        let binding = module.bind_import_typed("env", "shout", move |args, memory| {
            let (ptr, len, out, capacity) = match args {
                [
                    Val::I32(ptr),
                    Val::I32(len),
                    Val::I32(out),
                    Val::I32(capacity),
                ] => (*ptr, *len, *out, *capacity),
                _ => return Err(WasmError::Trap("host argument type")),
            };
            let name = guest_str(memory, ptr, len)?;
            let mut shouted = [0u8; 16];
            let count = name.len().min(shouted.len());
            shouted[..count].copy_from_slice(&name.as_bytes()[..count]);
            shouted[..count].make_ascii_uppercase();
            let written =
                guest_write(memory, out, reply_capacity.min(capacity), &shouted[..count])?;
            Ok(vec![Val::I32(written as i32)])
        });
        must_ok(binding, "bind env.shout");
        let mut instance = must_ok(module.instantiate(), "instantiate shout module");
        let result = instance.invoke_by_name("run", &[])?;
        // The reply really landed in the guest's own memory.
        let view = instance.memory()?;
        assert_eq!(guest_bytes(&view, 16, 6).ok(), Some(&b"TINYVM"[..]));
        Ok(result)
    }

    assert!(matches!(run(8).as_deref(), Ok([Val::I32(6)])));

    // A buffer the guest under-sized becomes a trap the guest sees.
    assert_eq!(
        run(3).map_err(|error| error.message()).err(),
        Some("guest memory window too small"),
        "an undersized guest buffer must trap, not truncate"
    );
}
