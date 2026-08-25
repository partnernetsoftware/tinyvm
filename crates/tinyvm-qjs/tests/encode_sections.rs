//! The encoder's capability surface: which sections it can write, which
//! opcodes it can spell, and whether the bytes clear tinyvm's load gate.
//!
//! `encode` is a private module of the crate, so this file compiles it (and the
//! `ir` it translates from) a second time rather than reaching through the
//! public API. That is deliberate and it is where *all* of the encoder's tests
//! live, the LEB128 property tests included: the evidence that matters for an
//! encoder is a module going through `WasmModule::from_bytes_with` and coming
//! back byte-equal to a reference assembler, and neither the load gate nor
//! `wat` belongs in a unit test inside `src`. A one-line `pub mod encode` in
//! `lib.rs` would be tidier and belongs to whoever owns that file.
//!
//! Three kinds of assertion, in increasing strength:
//!
//! 1. the bytes are what they should be (LEB128 minimality, raw float bits),
//! 2. tinyvm accepts them and running them produces the right value,
//! 3. they are *byte-identical* to what `wat` assembles from the same module.
//!
//! (3) is the one that catches a shape tinyvm merely tolerates.

#![allow(dead_code)]

#[path = "../src/ast.rs"]
mod ast;
#[path = "../src/diag.rs"]
mod diag;
#[path = "../src/encode.rs"]
mod encode;
#[path = "../src/ir.rs"]
mod ir;

use encode::{
    BlockType, ConstExpr, DESCRIPTOR_FUNC, DESCRIPTOR_GLOBAL, DESCRIPTOR_MEMORY, Data, Element,
    ExportEntry, FuncBody, Global, ImportDesc, ImportEntry, Limits, MemOp, Op, Signature,
    TableType, ValueType,
};
use tinyvm::{Val, WasmModule};

// -- harness -------------------------------------------------------------------

/// `Result::unwrap` takes a `WasmError` now that it derives `Debug`. This
/// stays because naming the stage that refused reads better than the fault
/// alone does.
fn ok<T>(result: Result<T, tinyvm::WasmError>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(e) => panic!("{what}: {}", e.message()),
    }
}

/// A whole module, assembled section by section in the order the spec requires
/// -- which is *not* id order: the data-count section is id 12 and belongs
/// between element (9) and code (10). Empty sections are omitted, which is what
/// a reference assembler does and what makes the byte-identity tests possible.
#[derive(Default)]
struct Builder {
    types: Vec<Signature>,
    imports: Vec<ImportEntry>,
    functions: Vec<u32>,
    tables: Vec<TableType>,
    memories: Vec<Limits>,
    globals: Vec<Global>,
    exports: Vec<ExportEntry>,
    start: Option<u32>,
    elements: Vec<Element>,
    data_count: bool,
    bodies: Vec<FuncBody>,
    data: Vec<Data>,
}

impl Builder {
    fn finish(&self) -> Vec<u8> {
        let mut out = encode::HEADER.to_vec();
        if !self.types.is_empty() {
            encode::type_section(&mut out, &self.types);
        }
        if !self.imports.is_empty() {
            encode::import_section(&mut out, &self.imports);
        }
        if !self.functions.is_empty() {
            encode::function_section(&mut out, &self.functions);
        }
        if !self.tables.is_empty() {
            encode::table_section(&mut out, &self.tables);
        }
        if !self.memories.is_empty() {
            encode::memory_section(&mut out, &self.memories);
        }
        if !self.globals.is_empty() {
            encode::global_section(&mut out, &self.globals);
        }
        if !self.exports.is_empty() {
            encode::export_section(&mut out, &self.exports);
        }
        if let Some(func) = self.start {
            encode::start_section(&mut out, func);
        }
        if !self.elements.is_empty() {
            encode::element_section(&mut out, &self.elements);
        }
        if self.data_count {
            encode::data_count_section(&mut out, self.data.len() as u32);
        }
        if !self.bodies.is_empty() {
            encode::code_section(&mut out, &self.bodies);
        }
        if !self.data.is_empty() {
            encode::data_section(&mut out, &self.data);
        }
        out
    }

    /// Through the load gate, instantiated, `main` called. Panics with the
    /// engine's own diagnostic so a broken fixture reads as itself.
    fn run(&self, what: &str, args: &[Val]) -> Vec<Val> {
        let bytes = self.finish();
        let module = ok(
            WasmModule::from_bytes_with(&bytes, tinyvm::Limits::default()),
            &format!("{what}: load gate"),
        );
        let mut instance = ok(module.instantiate(), &format!("{what}: instantiate"));
        ok(
            instance.invoke_by_name("main", args),
            &format!("{what}: calling main"),
        )
    }

    fn run1(&self, what: &str) -> Val {
        match self.run(what, &[]).as_slice() {
            [value] => *value,
            other => panic!("{what}: expected one result, got {}", other.len()),
        }
    }
}

fn sig(params: &[ValueType], results: &[ValueType]) -> Signature {
    Signature {
        params: params.to_vec(),
        results: results.to_vec(),
    }
}

fn export(name: &str, descriptor: u8, index: u32) -> ExportEntry {
    ExportEntry {
        name: name.to_string(),
        descriptor,
        index,
    }
}

fn body(code: Vec<u8>) -> FuncBody {
    FuncBody {
        locals: Vec::new(),
        code,
    }
}

/// Load the bytes and say what the load gate said, without panicking. Used
/// where the *rejection* is the thing under test.
fn gate(bytes: &[u8]) -> Result<(), &'static str> {
    WasmModule::from_bytes_with(bytes, tinyvm::Limits::default())
        .map(|_| ())
        .map_err(|e| e.message())
}

fn i32_of(value: Val) -> i32 {
    match value {
        Val::I32(n) => n,
        _ => panic!("expected i32"),
    }
}

fn i64_of(value: Val) -> i64 {
    match value {
        Val::I64(n) => n,
        _ => panic!("expected i64"),
    }
}

fn f64_of(value: Val) -> f64 {
    match value {
        Val::F64(n) => n,
        _ => panic!("expected f64"),
    }
}

// -- LEB128 and raw immediates ---------------------------------------------------

fn uleb(value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode::unsigned(&mut out, value);
    out
}

fn sleb(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    encode::signed_32(&mut out, value);
    out
}

fn sleb64(value: i64) -> Vec<u8> {
    let mut out = Vec::new();
    encode::signed_64(&mut out, value);
    out
}

#[test]
fn unsigned_leb128_is_minimal() {
    assert_eq!(uleb(0), [0x00]);
    assert_eq!(uleb(1), [0x01]);
    assert_eq!(uleb(63), [0x3f]);
    assert_eq!(uleb(64), [0x40]);
    assert_eq!(uleb(127), [0x7f]);
    assert_eq!(uleb(128), [0x80, 0x01]);
    assert_eq!(uleb(624_485), [0xe5, 0x8e, 0x26]);
    assert_eq!(uleb(u32::MAX), [0xff, 0xff, 0xff, 0xff, 0x0f]);
}

#[test]
fn signed_leb128_is_minimal_and_round_trips() {
    assert_eq!(sleb(0), [0x00]);
    assert_eq!(sleb(1), [0x01]);
    assert_eq!(sleb(63), [0x3f]);
    // 64 needs a second byte: 0x40 alone has bit 6 set and would read back
    // as -64. This is the case a naive encoder gets wrong.
    assert_eq!(sleb(64), [0xc0, 0x00]);
    assert_eq!(sleb(-1), [0x7f]);
    assert_eq!(sleb(-64), [0x40]);
    assert_eq!(sleb(-65), [0xbf, 0x7f]);
    assert_eq!(sleb(-123_456), [0xc0, 0xbb, 0x78]);
    assert_eq!(sleb(i32::MAX), [0xff, 0xff, 0xff, 0xff, 0x07]);
    assert_eq!(sleb(i32::MIN), [0x80, 0x80, 0x80, 0x80, 0x78]);

    // Every encoding decodes back to what it came from, and none is
    // longer than it has to be.
    for value in [
        i32::MIN,
        i32::MIN + 1,
        -1_000_000,
        -128,
        -65,
        -64,
        -1,
        0,
        1,
        63,
        64,
        127,
        128,
        1_000_000,
        i32::MAX - 1,
        i32::MAX,
    ] {
        let bytes = sleb(value);
        assert_eq!(
            decode_signed(&bytes),
            i64::from(value),
            "round trip of {value}"
        );
        assert_minimal(i64::from(value), &bytes);
        assert_eq!(bytes, sleb64(i64::from(value)), "{value} widened to i64");
    }
}

#[test]
fn signed_leb128_i64_is_minimal_and_round_trips() {
    assert_eq!(sleb64(0), [0x00]);
    assert_eq!(sleb64(-1), [0x7f]);
    assert_eq!(sleb64(64), [0xc0, 0x00]);
    // The two extremes are the ones tinyvm's `leb_s64` polices: it accepts a
    // tenth byte only when that byte is 0x00 or 0x7f, so a non-minimal
    // encoding of a large magnitude is not merely wasteful, it is rejected.
    assert_eq!(
        sleb64(i64::MAX),
        [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]
    );
    assert_eq!(
        sleb64(i64::MIN),
        [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x7f]
    );

    for value in [
        i64::MIN,
        i64::MIN + 1,
        -9_007_199_254_740_993,
        i64::from(i32::MIN) - 1,
        i64::from(i32::MIN),
        -4_294_967_296,
        -65,
        -64,
        -1,
        0,
        1,
        63,
        64,
        4_294_967_295,
        4_294_967_296,
        i64::from(i32::MAX),
        i64::from(i32::MAX) + 1,
        9_007_199_254_740_993,
        i64::MAX - 1,
        i64::MAX,
    ] {
        let bytes = sleb64(value);
        assert!(
            bytes.len() <= 10,
            "{value} encoded in {} bytes",
            bytes.len()
        );
        assert_eq!(decode_signed(&bytes), value, "round trip of {value}");
        assert_minimal(value, &bytes);
    }
}

/// Every `i64` immediate the encoder can emit survives the load gate and comes
/// back out of the engine unchanged. This is the range property the byte-level
/// tests above imply, measured end to end instead of assumed.
#[test]
fn i64_constants_survive_the_load_gate_and_execute() {
    for value in [
        i64::MIN,
        i64::MIN + 1,
        -4_294_967_296,
        -1,
        0,
        1,
        4_294_967_296,
        i64::MAX - 1,
        i64::MAX,
    ] {
        let mut code = Vec::new();
        encode::i64_const(&mut code, value);
        let module = Builder {
            types: vec![sig(&[], &[ValueType::I64])],
            functions: vec![0],
            exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
            bodies: vec![body(code)],
            ..Builder::default()
        };
        assert_eq!(i64_of(module.run1(&format!("i64.const {value}"))), value);
    }
}

/// Floats are raw little-endian IEEE-754 bits, not LEB128. The payload of a
/// signalling NaN is preserved bit for bit, which is the case that catches an
/// encoder that round-trips through an arithmetic type.
#[test]
fn float_constants_are_raw_little_endian_bits() {
    let mut out = Vec::new();
    encode::f32_const(&mut out, 1.0);
    assert_eq!(out, [0x43, 0x00, 0x00, 0x80, 0x3f]);

    let mut out = Vec::new();
    encode::f64_const(&mut out, 1.0);
    assert_eq!(out, [0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f]);

    let mut out = Vec::new();
    encode::f64_const(&mut out, -0.0);
    assert_eq!(out, [0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80]);

    let mut out = Vec::new();
    encode::f64_const(&mut out, f64::from_bits(0x7ff4_0000_dead_beef));
    assert_eq!(out, [0x44, 0xef, 0xbe, 0xad, 0xde, 0x00, 0x00, 0xf4, 0x7f]);
}

/// A signed LEB128 encoding is minimal exactly when its last byte is not a
/// pure sign extension of the one before it: a trailing `0x00` after a byte
/// with bit 6 clear, or a trailing `0x7f` after a byte with bit 6 set,
/// could both be dropped without changing the value.
fn assert_minimal(value: i64, bytes: &[u8]) {
    if bytes.len() < 2 {
        return;
    }
    let last = bytes[bytes.len() - 1];
    let previous_is_negative = bytes[bytes.len() - 2] & 0x40 != 0;
    assert!(
        !(last == 0x00 && !previous_is_negative) && !(last == 0x7f && previous_is_negative),
        "{value} encoded with a redundant trailing byte: {bytes:02x?}"
    );
}

fn decode_signed(bytes: &[u8]) -> i64 {
    let mut result: i64 = 0;
    let mut shift = 0;
    for (i, byte) in bytes.iter().enumerate() {
        result |= i64::from(byte & 0x7f) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            assert_eq!(i, bytes.len() - 1, "continuation byte past the end");
            if shift < 64 && byte & 0x40 != 0 {
                result |= -1i64 << shift;
            }
            break;
        }
    }
    result
}

// -- the opcode table ------------------------------------------------------------

/// Every single-byte opcode the encoder can spell, with the `.wat` mnemonic it
/// must assemble to and the operand types it consumes. `Op::Unreachable` and
/// `Op::Return` are the two variants missing here, because both end a block's
/// reachability and so cannot sit in the middle of a sequence; they are covered
/// by `control_flow_executes` below.
const NULLARY: &[(Op, &str, &str, bool)] = &[
    (Op::Nop, "nop", "", false),
    (Op::Drop, "drop", "i32", false),
    (Op::Select, "select", "i32 i32 i32", true),
    (Op::I32Eqz, "i32.eqz", "i32", true),
    (Op::I32Eq, "i32.eq", "i32 i32", true),
    (Op::I32Ne, "i32.ne", "i32 i32", true),
    (Op::I32LtS, "i32.lt_s", "i32 i32", true),
    (Op::I32LtU, "i32.lt_u", "i32 i32", true),
    (Op::I32GtS, "i32.gt_s", "i32 i32", true),
    (Op::I32GtU, "i32.gt_u", "i32 i32", true),
    (Op::I32LeS, "i32.le_s", "i32 i32", true),
    (Op::I32LeU, "i32.le_u", "i32 i32", true),
    (Op::I32GeS, "i32.ge_s", "i32 i32", true),
    (Op::I32GeU, "i32.ge_u", "i32 i32", true),
    (Op::I64Eqz, "i64.eqz", "i64", true),
    (Op::I64Eq, "i64.eq", "i64 i64", true),
    (Op::I64Ne, "i64.ne", "i64 i64", true),
    (Op::I64LtS, "i64.lt_s", "i64 i64", true),
    (Op::I64LtU, "i64.lt_u", "i64 i64", true),
    (Op::I64GtS, "i64.gt_s", "i64 i64", true),
    (Op::I64GtU, "i64.gt_u", "i64 i64", true),
    (Op::I64LeS, "i64.le_s", "i64 i64", true),
    (Op::I64LeU, "i64.le_u", "i64 i64", true),
    (Op::I64GeS, "i64.ge_s", "i64 i64", true),
    (Op::I64GeU, "i64.ge_u", "i64 i64", true),
    (Op::F32Eq, "f32.eq", "f32 f32", true),
    (Op::F32Ne, "f32.ne", "f32 f32", true),
    (Op::F32Lt, "f32.lt", "f32 f32", true),
    (Op::F32Gt, "f32.gt", "f32 f32", true),
    (Op::F32Le, "f32.le", "f32 f32", true),
    (Op::F32Ge, "f32.ge", "f32 f32", true),
    (Op::F64Eq, "f64.eq", "f64 f64", true),
    (Op::F64Ne, "f64.ne", "f64 f64", true),
    (Op::F64Lt, "f64.lt", "f64 f64", true),
    (Op::F64Gt, "f64.gt", "f64 f64", true),
    (Op::F64Le, "f64.le", "f64 f64", true),
    (Op::F64Ge, "f64.ge", "f64 f64", true),
    (Op::I32Clz, "i32.clz", "i32", true),
    (Op::I32Ctz, "i32.ctz", "i32", true),
    (Op::I32Popcnt, "i32.popcnt", "i32", true),
    (Op::I32Add, "i32.add", "i32 i32", true),
    (Op::I32Sub, "i32.sub", "i32 i32", true),
    (Op::I32Mul, "i32.mul", "i32 i32", true),
    (Op::I32DivS, "i32.div_s", "i32 i32", true),
    (Op::I32DivU, "i32.div_u", "i32 i32", true),
    (Op::I32RemS, "i32.rem_s", "i32 i32", true),
    (Op::I32RemU, "i32.rem_u", "i32 i32", true),
    (Op::I32And, "i32.and", "i32 i32", true),
    (Op::I32Or, "i32.or", "i32 i32", true),
    (Op::I32Xor, "i32.xor", "i32 i32", true),
    (Op::I32Shl, "i32.shl", "i32 i32", true),
    (Op::I32ShrS, "i32.shr_s", "i32 i32", true),
    (Op::I32ShrU, "i32.shr_u", "i32 i32", true),
    (Op::I32Rotl, "i32.rotl", "i32 i32", true),
    (Op::I32Rotr, "i32.rotr", "i32 i32", true),
    (Op::I64Clz, "i64.clz", "i64", true),
    (Op::I64Ctz, "i64.ctz", "i64", true),
    (Op::I64Popcnt, "i64.popcnt", "i64", true),
    (Op::I64Add, "i64.add", "i64 i64", true),
    (Op::I64Sub, "i64.sub", "i64 i64", true),
    (Op::I64Mul, "i64.mul", "i64 i64", true),
    (Op::I64DivS, "i64.div_s", "i64 i64", true),
    (Op::I64DivU, "i64.div_u", "i64 i64", true),
    (Op::I64RemS, "i64.rem_s", "i64 i64", true),
    (Op::I64RemU, "i64.rem_u", "i64 i64", true),
    (Op::I64And, "i64.and", "i64 i64", true),
    (Op::I64Or, "i64.or", "i64 i64", true),
    (Op::I64Xor, "i64.xor", "i64 i64", true),
    (Op::I64Shl, "i64.shl", "i64 i64", true),
    (Op::I64ShrS, "i64.shr_s", "i64 i64", true),
    (Op::I64ShrU, "i64.shr_u", "i64 i64", true),
    (Op::I64Rotl, "i64.rotl", "i64 i64", true),
    (Op::I64Rotr, "i64.rotr", "i64 i64", true),
    (Op::F32Abs, "f32.abs", "f32", true),
    (Op::F32Neg, "f32.neg", "f32", true),
    (Op::F32Ceil, "f32.ceil", "f32", true),
    (Op::F32Floor, "f32.floor", "f32", true),
    (Op::F32Trunc, "f32.trunc", "f32", true),
    (Op::F32Nearest, "f32.nearest", "f32", true),
    (Op::F32Sqrt, "f32.sqrt", "f32", true),
    (Op::F32Add, "f32.add", "f32 f32", true),
    (Op::F32Sub, "f32.sub", "f32 f32", true),
    (Op::F32Mul, "f32.mul", "f32 f32", true),
    (Op::F32Div, "f32.div", "f32 f32", true),
    (Op::F32Min, "f32.min", "f32 f32", true),
    (Op::F32Max, "f32.max", "f32 f32", true),
    (Op::F32Copysign, "f32.copysign", "f32 f32", true),
    (Op::F64Abs, "f64.abs", "f64", true),
    (Op::F64Neg, "f64.neg", "f64", true),
    (Op::F64Ceil, "f64.ceil", "f64", true),
    (Op::F64Floor, "f64.floor", "f64", true),
    (Op::F64Trunc, "f64.trunc", "f64", true),
    (Op::F64Nearest, "f64.nearest", "f64", true),
    (Op::F64Sqrt, "f64.sqrt", "f64", true),
    (Op::F64Add, "f64.add", "f64 f64", true),
    (Op::F64Sub, "f64.sub", "f64 f64", true),
    (Op::F64Mul, "f64.mul", "f64 f64", true),
    (Op::F64Div, "f64.div", "f64 f64", true),
    (Op::F64Min, "f64.min", "f64 f64", true),
    (Op::F64Max, "f64.max", "f64 f64", true),
    (Op::F64Copysign, "f64.copysign", "f64 f64", true),
    (Op::I32WrapI64, "i32.wrap_i64", "i64", true),
    (Op::I32TruncF32S, "i32.trunc_f32_s", "f32", true),
    (Op::I32TruncF32U, "i32.trunc_f32_u", "f32", true),
    (Op::I32TruncF64S, "i32.trunc_f64_s", "f64", true),
    (Op::I32TruncF64U, "i32.trunc_f64_u", "f64", true),
    (Op::I64ExtendI32S, "i64.extend_i32_s", "i32", true),
    (Op::I64ExtendI32U, "i64.extend_i32_u", "i32", true),
    (Op::I64TruncF32S, "i64.trunc_f32_s", "f32", true),
    (Op::I64TruncF32U, "i64.trunc_f32_u", "f32", true),
    (Op::I64TruncF64S, "i64.trunc_f64_s", "f64", true),
    (Op::I64TruncF64U, "i64.trunc_f64_u", "f64", true),
    (Op::F32ConvertI32S, "f32.convert_i32_s", "i32", true),
    (Op::F32ConvertI32U, "f32.convert_i32_u", "i32", true),
    (Op::F32ConvertI64S, "f32.convert_i64_s", "i64", true),
    (Op::F32ConvertI64U, "f32.convert_i64_u", "i64", true),
    (Op::F32DemoteF64, "f32.demote_f64", "f64", true),
    (Op::F64ConvertI32S, "f64.convert_i32_s", "i32", true),
    (Op::F64ConvertI32U, "f64.convert_i32_u", "i32", true),
    (Op::F64ConvertI64S, "f64.convert_i64_s", "i64", true),
    (Op::F64ConvertI64U, "f64.convert_i64_u", "i64", true),
    (Op::F64PromoteF32, "f64.promote_f32", "f32", true),
    (Op::I32ReinterpretF32, "i32.reinterpret_f32", "f32", true),
    (Op::I64ReinterpretF64, "i64.reinterpret_f64", "f64", true),
    (Op::F32ReinterpretI32, "f32.reinterpret_i32", "i32", true),
    (Op::F64ReinterpretI64, "f64.reinterpret_i64", "i64", true),
    (Op::I32Extend8S, "i32.extend8_s", "i32", true),
    (Op::I32Extend16S, "i32.extend16_s", "i32", true),
    (Op::I64Extend8S, "i64.extend8_s", "i64", true),
    (Op::I64Extend16S, "i64.extend16_s", "i64", true),
    (Op::I64Extend32S, "i64.extend32_s", "i64", true),
];

/// Push a zero of each named operand type. `0` is a legal operand for every op
/// in the table, including the dividing ones: nothing here is executed, only
/// assembled and validated.
fn push_operands(code: &mut Vec<u8>, operands: &str) {
    for operand in operands.split_whitespace() {
        match operand {
            "i32" => encode::i32_const(code, 0),
            "i64" => encode::i64_const(code, 0),
            "f32" => encode::f32_const(code, 0.0),
            "f64" => encode::f64_const(code, 0.0),
            other => panic!("unknown operand type {other}"),
        }
    }
}

/// The strongest evidence available for an opcode table: assemble every entry
/// twice, once through this encoder and once through `wat`, and compare the
/// whole module byte for byte. A wrong byte in any of the 131 rows fails here,
/// and so does a wrong `drop`, a wrong constant encoding, or a code-section
/// length that did not account for the body growing past 127 bytes.
#[test]
fn every_nullary_opcode_matches_the_reference_assembler() {
    let mut seen = std::collections::BTreeSet::new();
    let mut code = Vec::new();
    let mut text = String::from("(module (func\n");
    for (op, mnemonic, operands, produces) in NULLARY {
        assert!(
            seen.insert(*op as u8),
            "opcode 0x{:02x} appears twice in the table ({mnemonic})",
            *op as u8
        );
        push_operands(&mut code, operands);
        encode::op(&mut code, *op);
        if *produces {
            encode::op(&mut code, Op::Drop);
        }
        for operand in operands.split_whitespace() {
            text.push_str(&format!("  {operand}.const 0\n"));
        }
        text.push_str(&format!("  {mnemonic}\n"));
        if *produces {
            text.push_str("  drop\n");
        }
    }
    text.push_str("))");

    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        bodies: vec![body(code)],
        ..Builder::default()
    }
    .finish();
    let theirs = wat::parse_str(&text).expect("reference assembler");
    assert_eq!(
        ours, theirs,
        "the opcode table diverged from the reference assembler"
    );
    ok(
        WasmModule::from_bytes_with(&ours, tinyvm::Limits::default()).map(|_| ()),
        "load gate",
    );
}

// -- arithmetic that runs ----------------------------------------------------------

#[test]
fn i64_arithmetic_executes() {
    // 6_000_000_000 does not fit in an i32, so this is only right if the i64
    // path is real all the way through: constant, division, comparison.
    let mut code = Vec::new();
    encode::i64_const(&mut code, 6_000_000_000);
    encode::i64_const(&mut code, 3);
    encode::op(&mut code, Op::I64DivS);
    encode::i64_const(&mut code, 1_000_000_000);
    encode::op(&mut code, Op::I64Mul);

    let module = Builder {
        types: vec![sig(&[], &[ValueType::I64])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(
        i64_of(module.run1("i64 arithmetic")),
        2_000_000_000_000_000_000
    );
}

#[test]
fn i64_conversions_and_comparisons_execute() {
    // (i64) -1 extended from i32, compared unsigned against 1: the unsigned
    // comparison is the one that distinguishes a real i64 from a widened i32.
    let mut code = Vec::new();
    encode::i32_const(&mut code, -1);
    encode::op(&mut code, Op::I64ExtendI32S);
    encode::i64_const(&mut code, 1);
    encode::op(&mut code, Op::I64GtU);

    let module = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(i32_of(module.run1("i64.gt_u")), 1);

    // ...and wrapping it back down gives -1 again.
    let mut code = Vec::new();
    encode::i64_const(&mut code, -1);
    encode::op(&mut code, Op::I32WrapI64);
    let module = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(i32_of(module.run1("i32.wrap_i64")), -1);
}

#[test]
fn f64_arithmetic_executes() {
    // 1/0 is Infinity and 0/0 is NaN in IEEE-754, which is exactly why the
    // integer path has to trap there and this one does not.
    let mut code = Vec::new();
    encode::f64_const(&mut code, 0.1);
    encode::f64_const(&mut code, 0.2);
    encode::op(&mut code, Op::F64Add);

    let module = Builder {
        types: vec![sig(&[], &[ValueType::F64])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(f64_of(module.run1("f64.add")), 0.1 + 0.2);

    let mut code = Vec::new();
    encode::f64_const(&mut code, 1.0);
    encode::f64_const(&mut code, 0.0);
    encode::op(&mut code, Op::F64Div);
    let module = Builder {
        types: vec![sig(&[], &[ValueType::F64])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(f64_of(module.run1("f64.div by zero")), f64::INFINITY);

    // f64 -> i64 -> f64 through the reinterpret pair is the identity on bits,
    // which is what a tagged-value representation relies on.
    let mut code = Vec::new();
    encode::f64_const(&mut code, -2.5);
    encode::op(&mut code, Op::I64ReinterpretF64);
    encode::op(&mut code, Op::F64ReinterpretI64);
    let module = Builder {
        types: vec![sig(&[], &[ValueType::F64])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(f64_of(module.run1("reinterpret round trip")), -2.5);
}

// -- control flow --------------------------------------------------------------------

#[test]
fn control_flow_executes() {
    // sum = 0; i = 1; loop { sum += i; i += 1; if i <= 10 continue }  ->  55
    //
    // One `block` wrapping one `loop`, a `br_if` back edge, a `br` forward
    // exit, and an early `return` after the loop that is never taken. Locals
    // 0 and 1 are declared by the code entry, not by the function type.
    let mut code = Vec::new();
    encode::block(&mut code, BlockType::Empty);
    encode::loop_(&mut code, BlockType::Empty);
    //   sum += i
    encode::local_get(&mut code, 0);
    encode::local_get(&mut code, 1);
    encode::op(&mut code, Op::I32Add);
    encode::local_set(&mut code, 0);
    //   i += 1
    encode::local_get(&mut code, 1);
    encode::i32_const(&mut code, 1);
    encode::op(&mut code, Op::I32Add);
    encode::local_tee(&mut code, 1);
    //   if i > 10 leave the block, else go round again
    encode::i32_const(&mut code, 10);
    encode::op(&mut code, Op::I32GtS);
    encode::br_if(&mut code, 1);
    encode::br(&mut code, 0);
    encode::end(&mut code);
    encode::end(&mut code);
    encode::local_get(&mut code, 0);
    encode::op(&mut code, Op::Return);
    // Unreachable code after a `return` still has to encode and validate.
    encode::op(&mut code, Op::Unreachable);

    let module = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![FuncBody {
            // sum starts at 0, i starts at 0 -- so the loop adds 0 first and
            // then 1..10, which is still 55.
            locals: vec![(2, ValueType::I32)],
            code,
        }],
        ..Builder::default()
    };
    assert_eq!(i32_of(module.run1("loop")), 55);
}

#[test]
fn if_else_with_a_result_type_executes() {
    // `if` with a value-typed block: the arms must agree, and the block type
    // is the inline `i32` encoding rather than a type index.
    let run = |input: i32| {
        let mut code = Vec::new();
        encode::i32_const(&mut code, input);
        encode::if_(&mut code, BlockType::Value(ValueType::I32));
        encode::i32_const(&mut code, 10);
        encode::else_(&mut code);
        encode::i32_const(&mut code, 20);
        encode::end(&mut code);
        Builder {
            types: vec![sig(&[], &[ValueType::I32])],
            functions: vec![0],
            exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
            bodies: vec![body(code)],
            ..Builder::default()
        }
        .run1("if/else")
    };
    assert_eq!(i32_of(run(1)), 10);
    assert_eq!(i32_of(run(0)), 20);
}

#[test]
fn br_table_executes() {
    // Three labelled blocks and a default. The default label is *outside* the
    // vector's count, which is the immediate that is easiest to get wrong.
    let run = |input: i32| {
        let mut code = Vec::new();
        encode::block(&mut code, BlockType::Empty); // depth 2 -> default
        encode::block(&mut code, BlockType::Empty); // depth 1
        encode::block(&mut code, BlockType::Empty); // depth 0
        encode::local_get(&mut code, 0);
        encode::br_table(&mut code, &[0, 1], 2);
        encode::end(&mut code);
        encode::i32_const(&mut code, 100);
        encode::op(&mut code, Op::Return);
        encode::end(&mut code);
        encode::i32_const(&mut code, 200);
        encode::op(&mut code, Op::Return);
        encode::end(&mut code);
        encode::i32_const(&mut code, 300);
        Builder {
            types: vec![sig(&[ValueType::I32], &[ValueType::I32])],
            functions: vec![0],
            exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
            bodies: vec![body(code)],
            ..Builder::default()
        }
        .run("br_table", &[Val::I32(input)])
    };
    assert_eq!(i32_of(run(0)[0]), 100);
    assert_eq!(i32_of(run(1)[0]), 200);
    assert_eq!(i32_of(run(7)[0]), 300);
}

/// A block type given as a *type index* is an `s33`, so index 64 is `c0 00`
/// and not the single byte `40` -- `40` is the reserved "no result" encoding.
/// tinyvm says so in `block_type`; this is the encoder holding up its end.
#[test]
fn a_block_type_index_is_signed_and_never_collides_with_empty() {
    let mut out = Vec::new();
    encode::block_type(&mut out, BlockType::Empty);
    assert_eq!(out, [0x40]);

    let mut out = Vec::new();
    encode::block_type(&mut out, BlockType::TypeIndex(0));
    assert_eq!(out, [0x00]);

    let mut out = Vec::new();
    encode::block_type(&mut out, BlockType::TypeIndex(63));
    assert_eq!(out, [0x3f]);

    let mut out = Vec::new();
    encode::block_type(&mut out, BlockType::TypeIndex(64));
    assert_eq!(
        out,
        [0xc0, 0x00],
        "index 64 must not encode as the empty byte"
    );

    let mut out = Vec::new();
    encode::block_type(&mut out, BlockType::TypeIndex(1000));
    assert_eq!(out, [0xe8, 0x07]);
}

/// A block whose type is a full function type: two operands in, one out. This
/// is the only way to give a block parameters, and it needs the type section to
/// carry a signature no function uses.
#[test]
fn a_multi_operand_block_type_executes() {
    let mut code = Vec::new();
    encode::i32_const(&mut code, 20);
    encode::i32_const(&mut code, 22);
    encode::block(&mut code, BlockType::TypeIndex(1));
    encode::op(&mut code, Op::I32Add);
    encode::end(&mut code);

    let module = Builder {
        types: vec![
            sig(&[], &[ValueType::I32]),
            sig(&[ValueType::I32, ValueType::I32], &[ValueType::I32]),
        ],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(i32_of(module.run1("block with params")), 42);
}

// -- memory, data, globals -------------------------------------------------------

#[test]
fn memory_and_data_sections_execute() {
    // One page of memory, "wasm" written at offset 8 by an active data
    // segment, read back a byte at a time.
    let mut code = Vec::new();
    encode::i32_const(&mut code, 0);
    encode::mem(&mut code, MemOp::I32Load8U, 8);
    encode::i32_const(&mut code, 0);
    encode::mem(&mut code, MemOp::I32Load8U, 11);
    encode::op(&mut code, Op::I32Add);

    let module = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        memories: vec![Limits { min: 1, max: None }],
        exports: vec![
            export("main", DESCRIPTOR_FUNC, 0),
            export("mem", DESCRIPTOR_MEMORY, 0),
        ],
        bodies: vec![body(code)],
        data: vec![Data::Active {
            memory: 0,
            offset: ConstExpr::I32(8),
            bytes: b"wasm".to_vec(),
        }],
        ..Builder::default()
    };
    assert_eq!(
        i32_of(module.run1("data segment")),
        i32::from(b'w') + i32::from(b'm')
    );
}

#[test]
fn i64_and_f64_memory_access_executes() {
    // An eight-byte store and load at their natural alignment, which is the
    // memarg tinyvm checks hardest.
    let mut code = Vec::new();
    encode::i32_const(&mut code, 16);
    encode::f64_const(&mut code, 1.5);
    encode::mem(&mut code, MemOp::F64Store, 0);
    encode::i32_const(&mut code, 16);
    encode::mem(&mut code, MemOp::I64Load, 0);

    let module = Builder {
        types: vec![sig(&[], &[ValueType::I64])],
        functions: vec![0],
        memories: vec![Limits {
            min: 1,
            max: Some(2),
        }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(
        i64_of(module.run1("f64 store, i64 load")),
        1.5f64.to_bits() as i64
    );
}

/// The natural alignment is both the default and the ceiling. Emitting the
/// ceiling is accepted; one above it is not, and the encoder cannot produce
/// that byte -- so the rejected module here is hand-assembled to show the
/// boundary is real rather than assumed.
#[test]
fn memarg_alignment_is_capped_at_the_natural_width() {
    let cases = [
        (MemOp::I32Load8U, 0u32),
        (MemOp::I32Load16U, 1),
        (MemOp::I32Load, 2),
        (MemOp::I64Load, 3),
        (MemOp::F64Load, 3),
        (MemOp::I64Store32, 2),
    ];
    for (mem_op, natural) in cases {
        assert_eq!(mem_op.natural_align(), natural, "{mem_op:?}");

        let mut out = Vec::new();
        encode::mem(&mut out, mem_op, 0);
        assert_eq!(out, [mem_op as u8, natural as u8, 0x00], "{mem_op:?}");

        // One above natural: the same bytes with the alignment bumped. Built
        // by hand, because `mem_aligned` will not emit it.
        let is_store = (mem_op as u8) >= 0x36;
        let mut code = Vec::new();
        encode::i32_const(&mut code, 0);
        if is_store {
            encode::i64_const(&mut code, 0);
        }
        code.push(mem_op as u8);
        code.push(natural as u8 + 1);
        code.push(0x00);
        if !is_store {
            encode::op(&mut code, Op::Drop);
        }
        let bytes = Builder {
            types: vec![sig(&[], &[])],
            functions: vec![0],
            memories: vec![Limits { min: 1, max: None }],
            bodies: vec![body(code)],
            ..Builder::default()
        }
        .finish();
        assert_eq!(
            gate(&bytes),
            Err("memory alignment exceeds natural alignment"),
            "{mem_op:?} at align {} should be rejected",
            natural + 1
        );
    }
}

#[test]
fn global_section_executes() {
    // One immutable global read, one mutable global written and read back.
    let mut code = Vec::new();
    encode::global_get(&mut code, 0);
    encode::i32_const(&mut code, 2);
    encode::global_set(&mut code, 1);
    encode::global_get(&mut code, 1);
    encode::op(&mut code, Op::I32Add);

    let module = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        globals: vec![
            Global {
                ty: ValueType::I32,
                mutable: false,
                init: ConstExpr::I32(40),
            },
            Global {
                ty: ValueType::I32,
                mutable: true,
                init: ConstExpr::I32(0),
            },
        ],
        exports: vec![
            export("main", DESCRIPTOR_FUNC, 0),
            export("answer", DESCRIPTOR_GLOBAL, 0),
        ],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(i32_of(module.run1("globals")), 42);
}

#[test]
fn globals_of_every_value_type_clear_the_load_gate() {
    for (ty, init) in [
        (ValueType::I32, ConstExpr::I32(-1)),
        (ValueType::I64, ConstExpr::I64(i64::MIN)),
        (ValueType::F32, ConstExpr::F32(-0.5)),
        (ValueType::F64, ConstExpr::F64(f64::INFINITY)),
        (ValueType::FuncRef, ConstExpr::RefNull(ValueType::FuncRef)),
        (
            ValueType::ExternRef,
            ConstExpr::RefNull(ValueType::ExternRef),
        ),
    ] {
        let bytes = Builder {
            types: vec![sig(&[], &[])],
            functions: vec![0],
            globals: vec![Global {
                ty,
                mutable: true,
                init,
            }],
            bodies: vec![body(Vec::new())],
            ..Builder::default()
        }
        .finish();
        assert_eq!(gate(&bytes), Ok(()), "global of type {ty:?}");
    }
}

/// A `ref.func` constant expression needs the function *declared*, and tinyvm
/// takes an export as a declaration (`from_bytes_with`). That is the
/// non-obvious part: without the export the same bytes are rejected with
/// "global initializer has undeclared ref.func".
#[test]
fn a_ref_func_global_needs_the_function_declared() {
    let with_export = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        globals: vec![Global {
            ty: ValueType::FuncRef,
            mutable: false,
            init: ConstExpr::RefFunc(0),
        }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(Vec::new())],
        ..Builder::default()
    }
    .finish();
    assert_eq!(gate(&with_export), Ok(()));

    let without_export = Builder {
        exports: Vec::new(),
        ..Builder {
            types: vec![sig(&[], &[])],
            functions: vec![0],
            globals: vec![Global {
                ty: ValueType::FuncRef,
                mutable: false,
                init: ConstExpr::RefFunc(0),
            }],
            bodies: vec![body(Vec::new())],
            ..Builder::default()
        }
    }
    .finish();
    assert_eq!(
        gate(&without_export),
        Err("global initializer has undeclared ref.func")
    );
}

// -- start, tables, call_indirect --------------------------------------------------

#[test]
fn the_start_section_runs_before_main() {
    // `start` must be `[] -> []`. It writes 7 into a mutable global; `main`
    // only reads it, so a 7 proves the start function ran at instantiation.
    let mut start_code = Vec::new();
    encode::i32_const(&mut start_code, 7);
    encode::global_set(&mut start_code, 0);

    let mut main_code = Vec::new();
    encode::global_get(&mut main_code, 0);

    let module = Builder {
        types: vec![sig(&[], &[]), sig(&[], &[ValueType::I32])],
        functions: vec![0, 1],
        globals: vec![Global {
            ty: ValueType::I32,
            mutable: true,
            init: ConstExpr::I32(0),
        }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 1)],
        start: Some(0),
        bodies: vec![body(start_code), body(main_code)],
        ..Builder::default()
    };
    assert_eq!(i32_of(module.run1("start section")), 7);
}

#[test]
fn call_indirect_through_a_table_executes() {
    // Two functions of the same type in a table, dispatched on `main`'s
    // argument. `call_indirect` names the *type* index and the *table* index,
    // in that order.
    let mut double = Vec::new();
    encode::local_get(&mut double, 0);
    encode::i32_const(&mut double, 2);
    encode::op(&mut double, Op::I32Mul);

    let mut negate = Vec::new();
    encode::i32_const(&mut negate, 0);
    encode::local_get(&mut negate, 0);
    encode::op(&mut negate, Op::I32Sub);

    // main(which, value) -> value dispatched through table slot `which`
    let mut main_code = Vec::new();
    encode::local_get(&mut main_code, 1);
    encode::local_get(&mut main_code, 0);
    encode::call_indirect(&mut main_code, 0, 0);

    let unary = sig(&[ValueType::I32], &[ValueType::I32]);
    let module = Builder {
        types: vec![
            unary.clone(),
            sig(&[ValueType::I32, ValueType::I32], &[ValueType::I32]),
        ],
        functions: vec![0, 0, 1],
        tables: vec![TableType {
            element: ValueType::FuncRef,
            limits: Limits {
                min: 2,
                max: Some(2),
            },
        }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 2)],
        elements: vec![Element::ActiveFuncs {
            table: 0,
            offset: ConstExpr::I32(0),
            funcs: vec![0, 1],
        }],
        bodies: vec![body(double), body(negate), body(main_code)],
        ..Builder::default()
    };
    assert_eq!(
        i32_of(module.run("call_indirect", &[Val::I32(0), Val::I32(21)])[0]),
        42
    );
    assert_eq!(
        i32_of(module.run("call_indirect", &[Val::I32(1), Val::I32(21)])[0]),
        -21
    );
}

// -- locals ------------------------------------------------------------------------

/// Locals are declared as run-length groups, and the groups do not include the
/// parameters -- those are already locals `0..n`. Getting that wrong shifts
/// every index, so this checks the bytes and then checks that the engine
/// agrees about which local is which.
#[test]
fn local_declarations_are_run_length_groups_after_the_parameters() {
    let mut out = Vec::new();
    encode::locals(&mut out, &[]);
    assert_eq!(out, [0x00]);

    let mut out = Vec::new();
    encode::locals(
        &mut out,
        &[
            (2, ValueType::I64),
            (1, ValueType::F64),
            (300, ValueType::I32),
        ],
    );
    assert_eq!(
        out,
        [0x03, 0x02, 0x7e, 0x01, 0x7c, 0xac, 0x02, 0x7f],
        "three groups: 2 x i64, 1 x f64, 300 x i32"
    );

    // main(p: i32) with locals (i64, f64): local 0 is the parameter, 1 is the
    // i64, 2 is the f64.
    let mut code = Vec::new();
    encode::i64_const(&mut code, 5);
    encode::local_set(&mut code, 1);
    encode::f64_const(&mut code, 2.5);
    encode::local_set(&mut code, 2);
    encode::local_get(&mut code, 0);
    encode::op(&mut code, Op::F64ConvertI32S);
    encode::local_get(&mut code, 2);
    encode::op(&mut code, Op::F64Mul);
    encode::local_get(&mut code, 1);
    encode::op(&mut code, Op::F64ConvertI64S);
    encode::op(&mut code, Op::F64Add);

    let module = Builder {
        types: vec![sig(&[ValueType::I32], &[ValueType::F64])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![FuncBody {
            locals: vec![(1, ValueType::I64), (1, ValueType::F64)],
            code,
        }],
        ..Builder::default()
    };
    match module.run("locals", &[Val::I32(4)]).as_slice() {
        [value] => assert_eq!(f64_of(*value), 15.0),
        other => panic!("expected one result, got {}", other.len()),
    }
}

// -- custom sections -----------------------------------------------------------------

/// tinyvm reads a custom section's name and requires it to be valid UTF-8 that
/// stays inside the section. A custom section may also appear anywhere, which
/// is why it is exempt from the ordering rank.
#[test]
fn a_custom_section_carries_a_utf8_name() {
    let mut bytes = encode::HEADER.to_vec();
    encode::custom_section(&mut bytes, "tinyvm-qjs", b"\x00\xff\x01");
    encode::type_section(&mut bytes, &[sig(&[], &[])]);
    encode::function_section(&mut bytes, &[0]);
    encode::custom_section(&mut bytes, "", b"");
    encode::code_section(&mut bytes, &[body(Vec::new())]);
    encode::custom_section(&mut bytes, "\u{1f9ea}", b"trailing");
    assert_eq!(gate(&bytes), Ok(()));
}

// -- the whole thing, against the reference assembler ---------------------------------

/// The strongest single piece of evidence in this file: a module using every
/// section this encoder can write -- type, function, table, memory, global,
/// export, start, element, code, data -- with an i64/f64/control-flow body,
/// assembled twice and compared byte for byte.
#[test]
fn a_module_using_every_section_is_byte_identical_to_the_reference_assembler() {
    let mut start_code = Vec::new();
    encode::i32_const(&mut start_code, 1);
    encode::global_set(&mut start_code, 0);

    let mut helper = Vec::new();
    encode::local_get(&mut helper, 0);
    encode::op(&mut helper, Op::I64ExtendI32S);

    let mut main_code = Vec::new();
    encode::global_get(&mut main_code, 0);
    encode::if_(&mut main_code, BlockType::Value(ValueType::I64));
    encode::i32_const(&mut main_code, 0);
    encode::mem(&mut main_code, MemOp::I64Load, 8);
    encode::i64_const(&mut main_code, -9_007_199_254_740_993);
    encode::op(&mut main_code, Op::I64Add);
    encode::else_(&mut main_code);
    encode::f64_const(&mut main_code, 6.25);
    encode::op(&mut main_code, Op::I64TruncF64S);
    encode::end(&mut main_code);
    encode::local_set(&mut main_code, 1);
    encode::block(&mut main_code, BlockType::Empty);
    encode::local_get(&mut main_code, 1);
    encode::i64_const(&mut main_code, 0);
    encode::op(&mut main_code, Op::I64LtS);
    encode::br_if(&mut main_code, 0);
    encode::i32_const(&mut main_code, 0);
    encode::br_table(&mut main_code, &[0, 0], 0);
    encode::end(&mut main_code);
    encode::local_get(&mut main_code, 0);
    encode::i32_const(&mut main_code, 0);
    encode::call_indirect(&mut main_code, 2, 0);
    encode::local_get(&mut main_code, 1);
    encode::op(&mut main_code, Op::I64Mul);

    let ours = Builder {
        types: vec![
            sig(&[], &[]),
            sig(&[ValueType::I32], &[ValueType::I64]),
            sig(&[ValueType::I32], &[ValueType::I64]),
        ],
        functions: vec![0, 1, 1],
        tables: vec![TableType {
            element: ValueType::FuncRef,
            limits: Limits {
                min: 1,
                max: Some(1),
            },
        }],
        memories: vec![Limits {
            min: 1,
            max: Some(4),
        }],
        globals: vec![Global {
            ty: ValueType::I32,
            mutable: true,
            init: ConstExpr::I32(0),
        }],
        exports: vec![
            export("main", DESCRIPTOR_FUNC, 2),
            export("memory", DESCRIPTOR_MEMORY, 0),
            export("flag", DESCRIPTOR_GLOBAL, 0),
        ],
        start: Some(0),
        elements: vec![Element::ActiveFuncs {
            table: 0,
            offset: ConstExpr::I32(0),
            funcs: vec![1],
        }],
        bodies: vec![
            body(start_code),
            body(helper),
            FuncBody {
                locals: vec![(1, ValueType::I64)],
                code: main_code,
            },
        ],
        data: vec![Data::Active {
            memory: 0,
            offset: ConstExpr::I32(8),
            bytes: b"tinyvm-qjs".to_vec(),
        }],
        ..Builder::default()
    };

    let theirs = wat::parse_str(
        r#"(module
             (type (func))
             (type (func (param i32) (result i64)))
             (type (func (param i32) (result i64)))
             (table 1 1 funcref)
             (memory 1 4)
             (global (mut i32) (i32.const 0))
             (export "main" (func 2))
             (export "memory" (memory 0))
             (export "flag" (global 0))
             (start 0)
             (elem (i32.const 0) 1)
             (func (type 0)
               i32.const 1
               global.set 0)
             (func (type 1)
               local.get 0
               i64.extend_i32_s)
             (func (type 1) (local i64)
               global.get 0
               if (result i64)
                 i32.const 0
                 i64.load offset=8
                 i64.const -9007199254740993
                 i64.add
               else
                 f64.const 6.25
                 i64.trunc_f64_s
               end
               local.set 1
               block
                 local.get 1
                 i64.const 0
                 i64.lt_s
                 br_if 0
                 i32.const 0
                 br_table 0 0 0
               end
               local.get 0
               i32.const 0
               call_indirect (type 2)
               local.get 1
               i64.mul)
             (data (i32.const 8) "tinyvm-qjs"))"#,
    )
    .expect("reference assembler");

    assert_eq!(
        ours.finish(),
        theirs,
        "the encoder diverged from the reference assembler"
    );
    // And it runs. The start function sets the flag, so the `if` arm is taken:
    // load the eight data bytes at offset 8, add the i64 constant, then
    // multiply by what the indirect call sign-extends out of the parameter.
    let loaded = i64::from_le_bytes(*b"tinyvm-q");
    let expected = loaded.wrapping_add(-9_007_199_254_740_993).wrapping_mul(3);
    assert_eq!(i64_of(ours.run("everything", &[Val::I32(3)])[0]), expected);
}

// -- the paths the sections above left over -------------------------------------------

/// Passive segments, and the data-count section that has to precede the code
/// section when one exists. tinyvm cross-checks the declared count against the
/// data section it later reads and rejects a disagreement, so this is also the
/// test that the two are emitted in the right *order* -- data count is id 12
/// but belongs between element (9) and code (10).
#[test]
fn passive_segments_and_the_data_count_section_clear_the_load_gate() {
    let module = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        tables: vec![TableType {
            element: ValueType::FuncRef,
            limits: Limits { min: 1, max: None },
        }],
        memories: vec![Limits { min: 1, max: None }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        elements: vec![Element::PassiveFuncs(vec![0])],
        data_count: true,
        bodies: vec![body(Vec::new())],
        data: vec![
            Data::Passive(b"one".to_vec()),
            Data::Passive(b"two".to_vec()),
        ],
        ..Builder::default()
    };
    assert_eq!(gate(&module.finish()), Ok(()));

    // A count that disagrees with the section is the failure the data-count
    // section exists to make cheap, and the load gate does catch it.
    let mut wrong = encode::HEADER.to_vec();
    encode::type_section(&mut wrong, &[sig(&[], &[])]);
    encode::function_section(&mut wrong, &[0]);
    encode::memory_section(&mut wrong, &[Limits { min: 1, max: None }]);
    encode::data_count_section(&mut wrong, 5);
    encode::code_section(&mut wrong, &[body(Vec::new())]);
    encode::data_section(&mut wrong, &[Data::Passive(b"one".to_vec())]);
    assert_eq!(gate(&wrong), Err("data count does not match data section"));
}

/// An element segment naming a table other than 0, and a data segment naming a
/// memory other than 0. Both switch the encoder to the long segment form
/// (flag 2), which spells the index out and, for elements, adds an
/// element-kind byte the short form leaves implicit.
#[test]
fn segments_can_name_a_table_or_memory_other_than_zero() {
    let module = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        tables: vec![
            TableType {
                element: ValueType::FuncRef,
                limits: Limits { min: 1, max: None },
            },
            TableType {
                element: ValueType::FuncRef,
                limits: Limits { min: 4, max: None },
            },
        ],
        memories: vec![Limits { min: 1, max: None }, Limits { min: 1, max: None }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        elements: vec![Element::ActiveFuncs {
            table: 1,
            offset: ConstExpr::I32(2),
            funcs: vec![0],
        }],
        bodies: vec![body(Vec::new())],
        data: vec![Data::Active {
            memory: 1,
            offset: ConstExpr::I32(0),
            bytes: b"second".to_vec(),
        }],
        ..Builder::default()
    };
    assert_eq!(gate(&module.finish()), Ok(()));
}

/// `memory.size` and `memory.grow` carry a bare memory-index byte rather than
/// a LEB128 immediate, and an under-natural memarg is legal (it is only a
/// hint). Both are shapes a general encoder gets wrong in the same place.
#[test]
fn memory_size_grow_and_under_aligned_access_execute() {
    let mut code = Vec::new();
    // grow by one page, discard the old size, then report the new size...
    encode::i32_const(&mut code, 1);
    encode::memory_grow(&mut code, 0);
    encode::op(&mut code, Op::Drop);
    // ...after a deliberately under-aligned four-byte store and load.
    encode::i32_const(&mut code, 1);
    encode::i32_const(&mut code, 9);
    encode::mem_aligned(&mut code, MemOp::I32Store, 0, 0);
    encode::i32_const(&mut code, 1);
    encode::mem_aligned(&mut code, MemOp::I32Load, 0, 0);
    encode::memory_size(&mut code, 0);
    encode::op(&mut code, Op::I32Add);

    let module = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        memories: vec![Limits {
            min: 1,
            max: Some(4),
        }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    assert_eq!(i32_of(module.run1("memory.grow")), 9 + 2);
}

// -- imports beyond functions -----------------------------------------------------

/// A global constant expression may read another global, and tinyvm resolves
/// that index against the *imported* globals only, rejecting a mutable one
/// (`parse_const_expr`). A defined global therefore cannot initialise another
/// defined global -- which is the rule a lowering that hoists constants into
/// globals will hit first.
#[test]
fn a_global_initialiser_may_read_an_immutable_imported_global() {
    let importing = |mutable: bool| Builder {
        types: vec![sig(&[], &[])],
        imports: vec![ImportEntry {
            module: "js".to_string(),
            name: "base".to_string(),
            desc: ImportDesc::Global {
                ty: ValueType::I32,
                mutable,
            },
        }],
        functions: vec![0],
        globals: vec![Global {
            ty: ValueType::I32,
            mutable: false,
            init: ConstExpr::GlobalGet(0),
        }],
        bodies: vec![body(Vec::new())],
        ..Builder::default()
    };
    assert_eq!(gate(&importing(false).finish()), Ok(()));
    assert_eq!(
        gate(&importing(true).finish()),
        Err("const expr global index"),
        "a mutable imported global is not a constant"
    );

    // The same expression pointing at a *defined* global is rejected too: the
    // index space a const expr sees stops at the imports.
    let defined_only = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        globals: vec![
            Global {
                ty: ValueType::I32,
                mutable: false,
                init: ConstExpr::I32(1),
            },
            Global {
                ty: ValueType::I32,
                mutable: false,
                init: ConstExpr::GlobalGet(0),
            },
        ],
        bodies: vec![body(Vec::new())],
        ..Builder::default()
    };
    assert_eq!(gate(&defined_only.finish()), Err("const expr global index"));
}

/// Every import descriptor the encoder can write, in one module, checked
/// against the reference assembler. Imports occupy the low indices of their
/// index spaces, so `main` here is function 1, and the defined memory is
/// memory 1.
#[test]
fn every_import_descriptor_matches_the_reference_assembler() {
    let mut code = Vec::new();
    encode::global_get(&mut code, 0);
    encode::call(&mut code, 0);
    encode::op(&mut code, Op::I32Add);

    let ours = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        imports: vec![
            ImportEntry {
                module: "js".to_string(),
                name: "now".to_string(),
                desc: ImportDesc::Func(0),
            },
            ImportEntry {
                module: "js".to_string(),
                name: "table".to_string(),
                desc: ImportDesc::Table(TableType {
                    element: ValueType::ExternRef,
                    limits: Limits {
                        min: 1,
                        max: Some(9),
                    },
                }),
            },
            ImportEntry {
                module: "js".to_string(),
                name: "heap".to_string(),
                desc: ImportDesc::Memory(Limits { min: 2, max: None }),
            },
            ImportEntry {
                module: "js".to_string(),
                name: "base".to_string(),
                desc: ImportDesc::Global {
                    ty: ValueType::I32,
                    mutable: false,
                },
            },
        ],
        functions: vec![0],
        memories: vec![Limits { min: 1, max: None }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 1)],
        bodies: vec![body(code)],
        ..Builder::default()
    };

    let theirs = wat::parse_str(
        r#"(module
             (type (func (result i32)))
             (import "js" "now" (func (type 0)))
             (import "js" "table" (table 1 9 externref))
             (import "js" "heap" (memory 2))
             (import "js" "base" (global i32))
             (memory 1)
             (export "main" (func 1))
             (func (type 0)
               global.get 0
               call 0
               i32.add))"#,
    )
    .expect("reference assembler");
    assert_eq!(ours.finish(), theirs, "an import descriptor diverged");
    assert_eq!(gate(&ours.finish()), Ok(()));
}
