//! `Limits::max_decode_items` is a public ceiling.
//!
//! The default still refuses a module that needs more than 262,144 decode
//! items, an explicit larger ceiling loads the *same* bytes, and a zero or
//! tiny ceiling fails closed. The module here is hand-encoded so the three
//! arms share one artifact and the only variable is the ceiling.

use tinyvm::{Limits, WasmModule};

const NOP: u8 = 0x01;

fn leb(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// A valid module with one `() -> ()` function whose body is `nops` no-ops.
fn module_with_nops(nops: usize) -> Vec<u8> {
    let mut wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    // type section: one `() -> ()`
    wasm.push(0x01);
    wasm.push(4);
    wasm.extend_from_slice(&[0x01, 0x60, 0x00, 0x00]);

    // function section: one function of type 0
    wasm.push(0x03);
    wasm.push(2);
    wasm.extend_from_slice(&[0x01, 0x00]);

    // code section: one body, no locals, nops, end
    let mut body = Vec::with_capacity(nops + 8);
    body.push(0x00); // no local declarations
    body.extend(std::iter::repeat_n(NOP, nops));
    body.push(0x0b); // end

    let mut code = Vec::with_capacity(body.len() + 8);
    code.push(0x01); // one body
    leb(body.len(), &mut code);
    code.extend_from_slice(&body);

    wasm.push(0x0a);
    leb(code.len(), &mut wasm);
    wasm.extend_from_slice(&code);
    wasm
}

#[test]
fn the_default_ceiling_refuses_what_an_explicit_larger_one_loads() {
    let wasm = module_with_nops(300_000);

    let refused = WasmModule::from_bytes(&wasm)
        .err()
        .expect("the default 262,144-item ceiling must refuse 300,000 no-ops");
    assert!(
        format!("{refused:?}").contains("module decode budget"),
        "unexpected refusal: {refused:?}"
    );

    let raised = Limits {
        max_decode_items: 524_288,
        ..Limits::default()
    };
    WasmModule::from_bytes_with(&wasm, raised)
        .expect("an explicit 524,288-item ceiling must load the same bytes");
}

#[test]
fn a_zero_or_tiny_ceiling_fails_closed() {
    let wasm = module_with_nops(8);

    // The same bytes are fine under the default ceiling -- so the refusals
    // below are caused by the tiny ceiling alone, and must say so.
    WasmModule::from_bytes(&wasm).expect("8 no-ops load under the default ceiling");

    for ceiling in [0usize, 4] {
        let refused = WasmModule::from_bytes_with(
            &wasm,
            Limits {
                max_decode_items: ceiling,
                ..Limits::default()
            },
        )
        .err()
        .expect("a ceiling below the body's item count must refuse");
        assert!(
            format!("{refused:?}").contains("module decode budget"),
            "ceiling {ceiling} refused for the wrong reason: {refused:?}"
        );
    }
}
