//! Differential coverage for the encoder: every module it can build, assembled
//! a second time by `wat`, compared byte for byte, then loaded and run.
//!
//! `tests/encode_sections.rs` proves the encoder's *capability* surface and
//! carries one hand-written whole-module differential case, which passed the
//! first time it was run. That is evidence the encoder emits the canonical
//! shape rather than merely a shape tinyvm tolerates -- but it is one point of
//! evidence. This file is the systematic form of it: every section the encoder
//! can emit, every opcode that carries an immediate, and the LEB128 boundary
//! values where an encoding changes length.
//!
//! The byte comparison is the point. A non-canonical shape still clears the
//! load gate, so a byte difference is a finding even when both modules run, and
//! nothing here is allowed to degrade into "they behave the same". Where the
//! encoder is *documented* to diverge from a reference assembler by design, the
//! test says so, names the design decision, and asserts the divergence rather
//! than hiding it -- see `adjacent_local_groups_stay_unmerged`.
//!
//! Both byte strings go through `WasmModule::from_bytes_with`, and where the
//! module exports a `main` both are instantiated and run and their results
//! compared. While the bytes are equal that second half is redundant; it is
//! the half that keeps its meaning if they ever stop being.
//!
//! Like `encode_sections.rs`, this compiles `src/encode.rs` a second time
//! rather than reaching through the public API, because `encode` is a private
//! module. See that file's header for why that is the tests' problem and not
//! this one's.

#![allow(dead_code)]

#[path = "../src/encode.rs"]
mod encode;
#[path = "../src/ir.rs"]
mod ir;

use encode::{
    BlockType, ConstExpr, DESCRIPTOR_FUNC, DESCRIPTOR_GLOBAL, DESCRIPTOR_MEMORY, DESCRIPTOR_TABLE,
    Data, Element, ExportEntry, FuncBody, Global, ImportDesc, ImportEntry, Limits, MemOp, Op,
    Signature, TableType, ValueType,
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

/// A whole module, section by section in the order the spec requires -- which
/// is not id order: the data-count section is id 12 and belongs between element
/// (9) and code (10). Empty sections are omitted, because that is what a
/// reference assembler does and what makes byte identity a reachable bar.
#[derive(Default)]
struct Builder {
    custom_first: Vec<(String, Vec<u8>)>,
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
    custom_last: Vec<(String, Vec<u8>)>,
}

impl Builder {
    fn finish(&self) -> Vec<u8> {
        let mut out = encode::HEADER.to_vec();
        for (name, contents) in &self.custom_first {
            encode::custom_section(&mut out, name, contents);
        }
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
        for (name, contents) in &self.custom_last {
            encode::custom_section(&mut out, name, contents);
        }
        out
    }
}

/// The differential itself. Assemble the module twice -- once through this
/// encoder, once through `wat` -- require the two byte strings to be identical,
/// and require both to clear tinyvm's load gate. Returns the bytes.
fn agree(what: &str, ours: &Builder, text: &str) -> Vec<u8> {
    let ours = ours.finish();
    let theirs = match wat::parse_str(text) {
        Ok(bytes) => bytes,
        Err(e) => panic!("{what}: the reference assembler rejected the fixture text: {e}"),
    };
    if ours != theirs {
        panic!("{what}: {}", divergence(&ours, &theirs));
    }
    ok(
        WasmModule::from_bytes_with(&ours, tinyvm::Limits::default()).map(|_| ()),
        &format!("{what}: load gate, our bytes"),
    );
    ok(
        WasmModule::from_bytes_with(&theirs, tinyvm::Limits::default()).map(|_| ()),
        &format!("{what}: load gate, the reference assembler's bytes"),
    );
    ours
}

/// [`agree`], and then run `main` in *both* modules and require the same
/// results. Returns them.
fn agree_running(what: &str, ours: &Builder, text: &str, args: &[Val]) -> Vec<Val> {
    let bytes = agree(what, ours, text);
    let theirs = wat::parse_str(text).expect("already assembled once");
    let mine = invoke(&format!("{what}, our bytes"), &bytes, args);
    let reference = invoke(&format!("{what}, the reference assembler's"), &theirs, args);
    assert!(
        results_equal(&mine, &reference),
        "{what}: identical bytes ran to different results, {} against {}",
        describe(&mine),
        describe(&reference)
    );
    mine
}

fn invoke(what: &str, bytes: &[u8], args: &[Val]) -> Vec<Val> {
    let module = ok(
        WasmModule::from_bytes_with(bytes, tinyvm::Limits::default()),
        &format!("{what}: load gate"),
    );
    let mut instance = ok(module.instantiate(), &format!("{what}: instantiate"));
    ok(
        instance.invoke_by_name("main", args),
        &format!("{what}: calling main"),
    )
}

/// One `i32` result, which is what most of the executing fixtures return.
fn one_i32(values: &[Val]) -> i32 {
    match values {
        [Val::I32(n)] => *n,
        other => panic!("expected one i32 result, got {}", describe(other)),
    }
}

fn one_i64(values: &[Val]) -> i64 {
    match values {
        [Val::I64(n)] => *n,
        other => panic!("expected one i64 result, got {}", describe(other)),
    }
}

/// `Val` is `PartialEq` but not `Debug` outside tinyvm's own tests, so equality
/// is a comparison and reporting is this.
fn results_equal(left: &[Val], right: &[Val]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| match (a, b) {
            // NaN is not equal to itself, and two encoders agreeing on a NaN's
            // *bits* is exactly what the float fixtures are testing, so compare
            // floats by bits rather than by value.
            (Val::F32(a), Val::F32(b)) => a.to_bits() == b.to_bits(),
            (Val::F64(a), Val::F64(b)) => a.to_bits() == b.to_bits(),
            (a, b) => a == b,
        })
}

fn describe(values: &[Val]) -> String {
    let mut out = String::from("[");
    for (position, value) in values.iter().enumerate() {
        if position > 0 {
            out.push_str(", ");
        }
        match value {
            Val::I32(n) => out.push_str(&format!("i32 {n}")),
            Val::I64(n) => out.push_str(&format!("i64 {n}")),
            Val::F32(n) => out.push_str(&format!("f32 {:#010x}", n.to_bits())),
            Val::F64(n) => out.push_str(&format!("f64 {:#018x}", n.to_bits())),
            _ => out.push_str("(reference)"),
        }
    }
    out.push(']');
    out
}

/// Where the two byte strings first differ, with enough context on each side to
/// read the field that went wrong. A differential test whose failure says only
/// "not equal" is a test nobody can act on.
fn divergence(ours: &[u8], theirs: &[u8]) -> String {
    let at = ours
        .iter()
        .zip(theirs)
        .position(|(a, b)| a != b)
        .unwrap_or(ours.len().min(theirs.len()));
    let from = at.saturating_sub(8);
    let to = |bytes: &[u8]| (at + 9).min(bytes.len());
    format!(
        "the encoder diverged from the reference assembler at byte {at} \
         (ours {} bytes, theirs {} bytes)\n  ours   [{from}..]: {}\n  theirs [{from}..]: {}",
        ours.len(),
        theirs.len(),
        hex(&ours[from..to(ours)]),
        hex(&theirs[from..to(theirs)]),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// -- fixtures ------------------------------------------------------------------

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

/// The commonest shape here: one function, exported as `main`, returning one
/// value of `result` (or nothing, for the empty string).
fn main_only(result: &str, code: Vec<u8>) -> (Builder, String, String) {
    let results: Vec<ValueType> = match result {
        "" => vec![],
        "i32" => vec![ValueType::I32],
        "i64" => vec![ValueType::I64],
        "f32" => vec![ValueType::F32],
        "f64" => vec![ValueType::F64],
        other => panic!("unknown result type {other}"),
    };
    let builder = Builder {
        types: vec![sig(&[], &results)],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let head = if result.is_empty() {
        "(module (type (func)) (func (type 0)".to_string()
    } else {
        format!("(module (type (func (result {result}))) (func (type 0)")
    };
    (builder, head, "  ) (export \"main\" (func 0)))".to_string())
}

/// A float as text the reference assembler reads back to the same bits. Rust's
/// `Debug` for a finite float is shortest-round-trip, which is exactly the
/// guarantee needed; NaN and the infinities have no `Debug` form wat accepts,
/// so they are spelled in wat's own vocabulary, payload included.
fn wat_f64(value: f64) -> String {
    if value.is_nan() {
        let bits = value.to_bits();
        let sign = if bits >> 63 == 1 { "-" } else { "" };
        format!("{sign}nan:{:#x}", bits & ((1u64 << 52) - 1))
    } else if value.is_infinite() {
        if value < 0.0 {
            "-inf".into()
        } else {
            "inf".into()
        }
    } else {
        format!("{value:?}")
    }
}

fn wat_f32(value: f32) -> String {
    if value.is_nan() {
        let bits = value.to_bits();
        let sign = if bits >> 31 == 1 { "-" } else { "" };
        format!("{sign}nan:{:#x}", bits & ((1u32 << 23) - 1))
    } else if value.is_infinite() {
        if value < 0.0 {
            "-inf".into()
        } else {
            "inf".into()
        }
    } else {
        format!("{value:?}")
    }
}

/// The values where an unsigned LEB128 encoding changes length, and the ones on
/// either side of each step: 1 byte up to 127, 2 to 16383, 3 to 2097151, 4 to
/// 268435455, 5 beyond.
const UNSIGNED_BOUNDARIES: &[u32] = &[
    0,
    1,
    127,
    128,
    129,
    16_383,
    16_384,
    16_385,
    2_097_151,
    2_097_152,
    268_435_455,
    268_435_456,
    u32::MAX - 1,
    u32::MAX,
];

/// The same for signed LEB128 over an `i32`. A signed encoding steps at 2^6,
/// 2^13, 2^20 and 2^27 rather than at 2^7 and friends, because bit 6 of the
/// last byte carries the sign -- so 63 is one byte and 64 is two, and -64 is
/// one byte while -65 is two.
const SIGNED_32_BOUNDARIES: &[i32] = &[
    0,
    1,
    -1,
    63,
    64,
    -64,
    -65,
    127,
    128,
    8_191,
    8_192,
    -8_192,
    -8_193,
    16_383,
    16_384,
    1_048_575,
    1_048_576,
    -1_048_576,
    -1_048_577,
    134_217_727,
    134_217_728,
    -134_217_728,
    -134_217_729,
    i32::MAX - 1,
    i32::MAX,
    i32::MIN,
    i32::MIN + 1,
];

/// And over an `i64`, out to the tenth byte -- the one tinyvm range-checks.
const SIGNED_64_BOUNDARIES: &[i64] = &[
    0,
    1,
    -1,
    63,
    64,
    -64,
    -65,
    8_191,
    8_192,
    -8_192,
    -8_193,
    1_048_575,
    1_048_576,
    -1_048_576,
    -1_048_577,
    134_217_727,
    134_217_728,
    -134_217_728,
    -134_217_729,
    17_179_869_183,
    17_179_869_184,
    -17_179_869_184,
    -17_179_869_185,
    2_199_023_255_551,
    2_199_023_255_552,
    -2_199_023_255_552,
    -2_199_023_255_553,
    281_474_976_710_655,
    281_474_976_710_656,
    -281_474_976_710_656,
    -281_474_976_710_657,
    36_028_797_018_963_967,
    36_028_797_018_963_968,
    -36_028_797_018_963_968,
    -36_028_797_018_963_969,
    4_611_686_018_427_387_903,
    4_611_686_018_427_387_904,
    -4_611_686_018_427_387_904,
    -4_611_686_018_427_387_905,
    i64::MAX - 1,
    i64::MAX,
    i64::MIN,
    i64::MIN + 1,
    // The V1 representation's own extremes: the largest integer an f64 holds
    // exactly, and the first one past it.
    9_007_199_254_740_991,
    9_007_199_254_740_992,
    -9_007_199_254_740_993,
];

// -- the module skeleton -------------------------------------------------------

#[test]
fn an_empty_module_is_the_header_and_nothing_else() {
    let ours = Builder::default();
    let bytes = agree("the empty module", &ours, "(module)");
    assert_eq!(bytes, encode::HEADER, "no section should have been emitted");
}

// -- sections ------------------------------------------------------------------

#[test]
fn the_type_section_covers_every_value_type_and_arity() {
    // Every numeric type in a parameter position, every one in a result
    // position, the empty signature, and a multi-value result -- which is the
    // only arity a wasm 1.0 decoder would refuse.
    let ours = Builder {
        types: vec![
            sig(&[], &[]),
            sig(&[ValueType::I32], &[ValueType::I32]),
            sig(&[ValueType::I64], &[ValueType::I64]),
            sig(&[ValueType::F32], &[ValueType::F32]),
            sig(&[ValueType::F64], &[ValueType::F64]),
            sig(
                &[
                    ValueType::I32,
                    ValueType::I64,
                    ValueType::F32,
                    ValueType::F64,
                ],
                &[
                    ValueType::F64,
                    ValueType::F32,
                    ValueType::I64,
                    ValueType::I32,
                ],
            ),
            sig(&[], &[ValueType::I32, ValueType::I32]),
            sig(&[ValueType::FuncRef, ValueType::ExternRef], &[]),
        ],
        ..Builder::default()
    };
    agree(
        "the type section's value types",
        &ours,
        r#"(module
             (type (func))
             (type (func (param i32) (result i32)))
             (type (func (param i64) (result i64)))
             (type (func (param f32) (result f32)))
             (type (func (param f64) (result f64)))
             (type (func (param i32 i64 f32 f64) (result f64 f32 i64 i32)))
             (type (func (result i32 i32)))
             (type (func (param funcref externref))))"#,
    );
}

#[test]
fn the_type_section_count_crosses_the_leb_boundary() {
    // 127 entries is a one-byte count and 128 is two, and the section's own
    // length crosses the same step a few entries earlier. Both sides of both
    // steps, since an off-by-one in either direction is a different bug.
    for count in [126u32, 127, 128, 129, 16_383, 16_384] {
        let ours = Builder {
            types: (0..count).map(|_| sig(&[], &[])).collect(),
            ..Builder::default()
        };
        let mut text = String::from("(module ");
        for _ in 0..count {
            text.push_str("(type (func)) ");
        }
        text.push(')');
        agree(&format!("a type section of {count} entries"), &ours, &text);
    }
}

#[test]
fn the_import_section_covers_every_descriptor() {
    // All four descriptor bytes, both limits forms, both reference types, and
    // a global in each mutability -- the whole of what `ImportDesc` can say.
    let ours = Builder {
        types: vec![sig(&[], &[]), sig(&[ValueType::I32], &[ValueType::I64])],
        imports: vec![
            ImportEntry {
                module: "js".into(),
                name: "now".into(),
                desc: ImportDesc::Func(0),
            },
            ImportEntry {
                module: "js".into(),
                name: "convert".into(),
                desc: ImportDesc::Func(1),
            },
            ImportEntry {
                module: "js".into(),
                name: "funcs".into(),
                desc: ImportDesc::Table(TableType {
                    element: ValueType::FuncRef,
                    limits: Limits { min: 0, max: None },
                }),
            },
            ImportEntry {
                module: "js".into(),
                name: "handles".into(),
                desc: ImportDesc::Table(TableType {
                    element: ValueType::ExternRef,
                    limits: Limits {
                        min: 1,
                        max: Some(16_384),
                    },
                }),
            },
            ImportEntry {
                module: "js".into(),
                name: "heap".into(),
                desc: ImportDesc::Memory(Limits { min: 2, max: None }),
            },
            ImportEntry {
                module: "js".into(),
                name: "bounded".into(),
                desc: ImportDesc::Memory(Limits {
                    min: 0,
                    max: Some(0),
                }),
            },
            ImportEntry {
                module: "js".into(),
                name: "base".into(),
                desc: ImportDesc::Global {
                    ty: ValueType::I32,
                    mutable: false,
                },
            },
            ImportEntry {
                module: "js".into(),
                name: "cursor".into(),
                desc: ImportDesc::Global {
                    ty: ValueType::F64,
                    mutable: true,
                },
            },
            // An empty module name and an empty field name are both legal, and
            // both are a length byte a producer can get wrong.
            ImportEntry {
                module: String::new(),
                name: String::new(),
                desc: ImportDesc::Func(0),
            },
        ],
        ..Builder::default()
    };
    agree(
        "the import section's descriptors",
        &ours,
        r#"(module
             (type (func))
             (type (func (param i32) (result i64)))
             (import "js" "now" (func (type 0)))
             (import "js" "convert" (func (type 1)))
             (import "js" "funcs" (table 0 funcref))
             (import "js" "handles" (table 1 16384 externref))
             (import "js" "heap" (memory 2))
             (import "js" "bounded" (memory 0 0))
             (import "js" "base" (global i32))
             (import "js" "cursor" (global (mut f64)))
             (import "" "" (func (type 0))))"#,
    );
}

#[test]
fn the_table_section_covers_element_types_and_limits() {
    let ours = Builder {
        tables: vec![
            TableType {
                element: ValueType::FuncRef,
                limits: Limits { min: 0, max: None },
            },
            TableType {
                element: ValueType::FuncRef,
                limits: Limits {
                    min: 1,
                    max: Some(1),
                },
            },
            TableType {
                element: ValueType::ExternRef,
                limits: Limits {
                    min: 128,
                    max: Some(16_384),
                },
            },
        ],
        ..Builder::default()
    };
    agree(
        "the table section",
        &ours,
        r#"(module
             (table 0 funcref)
             (table 1 1 funcref)
             (table 128 16384 externref))"#,
    );
}

#[test]
fn the_memory_section_covers_every_limits_form() {
    // `min` with no maximum, `min` with one, a zero-page memory, and a maximum
    // whose LEB128 is two bytes. Flag `0x01` with `max == min` is deliberately
    // not the same bytes as flag `0x00`, which is why the second entry exists.
    let ours = Builder {
        memories: vec![
            Limits { min: 1, max: None },
            Limits {
                min: 1,
                max: Some(1),
            },
            Limits {
                min: 0,
                max: Some(0),
            },
            Limits {
                min: 0,
                max: Some(128),
            },
        ],
        ..Builder::default()
    };
    agree(
        "the memory section",
        &ours,
        r#"(module (memory 1) (memory 1 1) (memory 0 0) (memory 0 128))"#,
    );
}

#[test]
fn the_global_section_covers_every_const_expr() {
    // Every `ConstExpr` variant. `GlobalGet` resolves against the *imported*
    // globals only and refuses a mutable one, so the import above it is not
    // decoration.
    let ours = Builder {
        types: vec![sig(&[], &[])],
        imports: vec![ImportEntry {
            module: "js".into(),
            name: "base".into(),
            desc: ImportDesc::Global {
                ty: ValueType::I32,
                mutable: false,
            },
        }],
        functions: vec![0],
        globals: vec![
            Global {
                ty: ValueType::I32,
                mutable: false,
                init: ConstExpr::I32(-1),
            },
            Global {
                ty: ValueType::I32,
                mutable: true,
                init: ConstExpr::I32(i32::MIN),
            },
            Global {
                ty: ValueType::I64,
                mutable: false,
                init: ConstExpr::I64(i64::MIN),
            },
            Global {
                ty: ValueType::F32,
                mutable: false,
                init: ConstExpr::F32(-0.0),
            },
            Global {
                ty: ValueType::F64,
                mutable: true,
                init: ConstExpr::F64(f64::INFINITY),
            },
            Global {
                ty: ValueType::I32,
                mutable: false,
                init: ConstExpr::GlobalGet(0),
            },
            Global {
                ty: ValueType::FuncRef,
                mutable: false,
                init: ConstExpr::RefNull(ValueType::FuncRef),
            },
            Global {
                ty: ValueType::ExternRef,
                mutable: true,
                init: ConstExpr::RefNull(ValueType::ExternRef),
            },
            Global {
                ty: ValueType::FuncRef,
                mutable: false,
                init: ConstExpr::RefFunc(0),
            },
        ],
        // `ref.func` in a global initialiser is only legal for a function the
        // module has *declared*, and tinyvm counts an export as a declaration
        // (`crates/tinyvm/src/wasm.rs`, "a function export declares that
        // function for `ref.func`"). Without this export the load gate refuses
        // bytes the reference assembler happily produced.
        exports: vec![export("declared", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(Vec::new())],
        ..Builder::default()
    };
    agree(
        "the global section's initialisers",
        &ours,
        r#"(module
             (type (func))
             (import "js" "base" (global i32))
             (global i32 (i32.const -1))
             (global (mut i32) (i32.const -2147483648))
             (global i64 (i64.const -9223372036854775808))
             (global f32 (f32.const -0.0))
             (global (mut f64) (f64.const inf))
             (global i32 (global.get 0))
             (global funcref (ref.null func))
             (global (mut externref) (ref.null extern))
             (global funcref (ref.func 0))
             (export "declared" (func 0))
             (func (type 0)))"#,
    );
}

#[test]
fn the_export_section_covers_every_descriptor_and_name() {
    // The four descriptor bytes, an empty name, a name that is not ASCII, and a
    // name long enough that its length is two LEB128 bytes.
    let long: String = std::iter::repeat_n('n', 200).collect();
    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        tables: vec![TableType {
            element: ValueType::FuncRef,
            limits: Limits { min: 1, max: None },
        }],
        memories: vec![Limits { min: 1, max: None }],
        globals: vec![Global {
            ty: ValueType::I32,
            mutable: false,
            init: ConstExpr::I32(0),
        }],
        exports: vec![
            export("main", DESCRIPTOR_FUNC, 0),
            export("table", DESCRIPTOR_TABLE, 0),
            export("memory", DESCRIPTOR_MEMORY, 0),
            export("flag", DESCRIPTOR_GLOBAL, 0),
            export("", DESCRIPTOR_FUNC, 0),
            export("\u{e4}\u{4e2d}\u{1f600}", DESCRIPTOR_FUNC, 0),
            export(&long, DESCRIPTOR_FUNC, 0),
        ],
        bodies: vec![body(Vec::new())],
        ..Builder::default()
    };
    agree(
        "the export section's descriptors",
        &ours,
        &format!(
            r#"(module
                 (type (func))
                 (table 1 funcref)
                 (memory 1)
                 (global i32 (i32.const 0))
                 (export "main" (func 0))
                 (export "table" (table 0))
                 (export "memory" (memory 0))
                 (export "flag" (global 0))
                 (export "" (func 0))
                 (export "\u{{e4}}\u{{4e2d}}\u{{1f600}}" (func 0))
                 (export "{long}" (func 0))
                 (func (type 0)))"#
        ),
    );
}

#[test]
fn the_export_section_count_crosses_the_leb_boundary() {
    for count in [127u32, 128] {
        let ours = Builder {
            types: vec![sig(&[], &[])],
            functions: vec![0],
            exports: (0..count)
                .map(|i| export(&format!("e{i}"), DESCRIPTOR_FUNC, 0))
                .collect(),
            bodies: vec![body(Vec::new())],
            ..Builder::default()
        };
        let mut text = String::from("(module (type (func)) ");
        for i in 0..count {
            text.push_str(&format!("(export \"e{i}\" (func 0)) "));
        }
        text.push_str("(func (type 0)))");
        agree(&format!("{count} exports"), &ours, &text);
    }
}

#[test]
fn the_start_section_matches_and_the_module_runs() {
    // The start function runs before any export can be called, so a global it
    // sets is observable from `main` -- which is the only way to tell a start
    // section that was decoded from one that was skipped.
    let mut start_code = Vec::new();
    encode::i32_const(&mut start_code, 42);
    encode::global_set(&mut start_code, 0);
    let mut main_code = Vec::new();
    encode::global_get(&mut main_code, 0);

    let ours = Builder {
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
    let results = agree_running(
        "the start section",
        &ours,
        r#"(module
             (type (func))
             (type (func (result i32)))
             (global (mut i32) (i32.const 0))
             (export "main" (func 1))
             (start 0)
             (func (type 0) i32.const 42 global.set 0)
             (func (type 1) global.get 0))"#,
        &[],
    );
    assert_eq!(one_i32(&results), 42, "the start function did not run");
}

#[test]
fn the_element_section_covers_all_three_flags() {
    // Flag 0 (active, table 0, implicit funcref), flag 1 (passive, explicit
    // element kind) and flag 2 (active, explicit table index). Flag 2 needs a
    // second table for its index to mean anything.
    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0, 0],
        tables: vec![
            TableType {
                element: ValueType::FuncRef,
                limits: Limits { min: 4, max: None },
            },
            TableType {
                element: ValueType::FuncRef,
                limits: Limits { min: 4, max: None },
            },
        ],
        elements: vec![
            Element::ActiveFuncs {
                table: 0,
                offset: ConstExpr::I32(0),
                funcs: vec![0, 1],
            },
            Element::PassiveFuncs(vec![1, 0]),
            Element::ActiveFuncs {
                table: 1,
                offset: ConstExpr::I32(2),
                funcs: vec![0],
            },
            // An empty segment is a vector whose count is zero, not an absent
            // vector, and the two are different bytes.
            Element::PassiveFuncs(Vec::new()),
        ],
        bodies: vec![body(Vec::new()), body(Vec::new())],
        ..Builder::default()
    };
    agree(
        "the element section's three flags",
        &ours,
        r#"(module
             (type (func))
             (table 4 funcref)
             (table 4 funcref)
             (elem (i32.const 0) 0 1)
             (elem func 1 0)
             (elem (table 1) (i32.const 2) func 0)
             (elem func)
             (func (type 0))
             (func (type 0)))"#,
    );
}

#[test]
fn the_data_section_covers_all_three_flags() {
    let ours = Builder {
        memories: vec![Limits { min: 1, max: None }, Limits { min: 1, max: None }],
        data: vec![
            Data::Active {
                memory: 0,
                offset: ConstExpr::I32(8),
                bytes: b"tinyvm-qjs".to_vec(),
            },
            Data::Passive(b"passive".to_vec()),
            Data::Active {
                memory: 1,
                offset: ConstExpr::I32(0),
                bytes: vec![0x00, 0xff, 0x80, 0x7f],
            },
            // Zero bytes at a zero offset: every length field in the entry is
            // at its minimum, which is where a missing one hides.
            Data::Active {
                memory: 0,
                offset: ConstExpr::I32(0),
                bytes: Vec::new(),
            },
            // ...and one whose length needs two LEB128 bytes.
            Data::Passive(vec![0x61; 300]),
        ],
        ..Builder::default()
    };
    let long: String = std::iter::repeat_n("a", 300).collect();
    agree(
        "the data section's three flags",
        &ours,
        &format!(
            r#"(module
                 (memory 1)
                 (memory 1)
                 (data (i32.const 8) "tinyvm-qjs")
                 (data "passive")
                 (data (memory 1) (i32.const 0) "\00\ff\80\7f")
                 (data (i32.const 0) "")
                 (data "{long}"))"#
        ),
    );
}

#[test]
fn the_data_count_section_matches_the_reference_assembler() {
    // The data-count section is required only by `memory.init` and `data.drop`,
    // and a reference assembler emits it only when the code uses one of them --
    // so the fixture has to use one. The encoder has no writer for the bulk
    // opcodes yet (`memory.init` is `fc 08`), so those four bytes are spelled
    // here by hand; every other byte in the module, the data-count section
    // included, is the encoder's.
    let mut code = Vec::new();
    encode::i32_const(&mut code, 0); // destination
    encode::i32_const(&mut code, 0); // source offset
    encode::i32_const(&mut code, 3); // length
    code.extend_from_slice(&[0xfc, 0x08, 0x00, 0x00]); // memory.init 0, memory 0

    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        memories: vec![Limits { min: 1, max: None }],
        data_count: true,
        bodies: vec![body(code)],
        data: vec![Data::Passive(b"abc".to_vec())],
        ..Builder::default()
    };
    agree(
        "the data-count section",
        &ours,
        r#"(module
             (type (func))
             (memory 1)
             (data "abc")
             (func (type 0) i32.const 0 i32.const 0 i32.const 3 memory.init 0))"#,
    );
}

#[test]
fn a_custom_section_matches_at_either_end_of_the_module() {
    // A custom section is legal anywhere, and where it sits is part of the
    // bytes. One before the type section and one after the code section pins
    // both ends.
    let ours = Builder {
        custom_first: vec![("before".into(), b"head".to_vec())],
        types: vec![sig(&[], &[])],
        functions: vec![0],
        bodies: vec![body(Vec::new())],
        custom_last: vec![
            ("after".into(), b"tail".to_vec()),
            // An empty name and empty contents: two length fields at zero.
            (String::new(), Vec::new()),
        ],
        ..Builder::default()
    };
    agree(
        "custom sections at both ends",
        &ours,
        r#"(module
             (@custom "before" (before type) "head")
             (type (func))
             (func (type 0))
             (@custom "after" "tail")
             (@custom "" ""))"#,
    );
}

#[test]
fn the_name_section_matches_what_a_reference_assembler_writes() {
    // `ir::m1::assemble` builds a `name` custom section out of `custom_section`,
    // `vector`, `unsigned` and `name`. Its shape is a subsection id, a byte
    // length, then a vector of (function index, name) -- and the only way to
    // know that is right is to make an assembler write the same thing.
    let mut map = Vec::new();
    encode::vector(
        &mut map,
        &[(0u32, "start"), (1, "add")],
        |body, (i, text)| {
            encode::unsigned(body, *i);
            encode::name(body, text);
        },
    );
    let mut contents = vec![1u8]; // subsection 1: the function-name map
    encode::unsigned(&mut contents, map.len() as u32);
    contents.extend_from_slice(&map);

    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0, 0],
        bodies: vec![body(Vec::new()), body(Vec::new())],
        custom_last: vec![("name".into(), contents)],
        ..Builder::default()
    };
    agree(
        "the name custom section",
        &ours,
        r#"(module
             (type (func))
             (func $start (type 0))
             (func $add (type 0)))"#,
    );
}

// -- the code section ------------------------------------------------------------

#[test]
fn locals_groups_match_when_the_caller_already_merged_them() {
    let ours = Builder {
        types: vec![sig(&[ValueType::I32], &[])],
        functions: vec![0],
        bodies: vec![FuncBody {
            locals: vec![
                (2, ValueType::I32),
                (1, ValueType::I64),
                (3, ValueType::F64),
                (1, ValueType::F32),
            ],
            code: Vec::new(),
        }],
        ..Builder::default()
    };
    agree(
        "merged local groups",
        &ours,
        r#"(module
             (type (func (param i32)))
             (func (type 0) (local i32 i32) (local i64) (local f64 f64 f64) (local f32)))"#,
    );
}

#[test]
fn adjacent_local_groups_stay_unmerged() {
    // The one place the encoder is *documented* to diverge from a reference
    // assembler: `encode::locals` does not merge adjacent groups of the same
    // type, because only the caller knows which of the two byte strings it
    // meant. `wat` always merges. That is a design decision, not a defect, so
    // the test asserts the divergence and checks that both forms load -- which
    // is the evidence that the choice is free.
    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        bodies: vec![FuncBody {
            locals: vec![(1, ValueType::I32), (1, ValueType::I32)],
            code: Vec::new(),
        }],
        ..Builder::default()
    };
    let unmerged = ours.finish();
    let merged = wat::parse_str("(module (type (func)) (func (type 0) (local i32) (local i32)))")
        .expect("reference assembler");
    assert_ne!(
        unmerged, merged,
        "the reference assembler stopped merging adjacent local groups, so the \
         encoder's decision not to merge is no longer a divergence and this test \
         has nothing left to say"
    );
    // Two groups of one against one group of two: same locals, two bytes more.
    assert_eq!(unmerged.len(), merged.len() + 2);
    assert_eq!(gate(&unmerged), Ok(()), "the unmerged form must still load");
    assert_eq!(gate(&merged), Ok(()));
}

fn gate(bytes: &[u8]) -> Result<(), &'static str> {
    WasmModule::from_bytes_with(bytes, tinyvm::Limits::default())
        .map(|_| ())
        .map_err(|e| e.message())
}

#[test]
fn a_code_entry_size_crosses_the_leb_boundary() {
    // The size prefix on a code entry is measured, not predicted, and an entry
    // of exactly 127 bytes is where a producer that predicted it goes wrong.
    // An entry is the locals vector (one byte here) plus the body plus `end`,
    // so `nops + 2` is the size being stepped across.
    for nops in [0usize, 125, 126, 127, 16_381, 16_382] {
        let mut code = Vec::new();
        for _ in 0..nops {
            encode::op(&mut code, Op::Nop);
        }
        let (ours, head, tail) = main_only("", code);
        let mut text = head;
        for _ in 0..nops {
            text.push_str(" nop");
        }
        text.push_str(&tail);
        let bytes = agree(&format!("a code entry of {nops} nops"), &ours, &text);
        assert!(bytes.len() > nops, "the body did not reach the output");
        agree_running(&format!("running {nops} nops"), &ours, &text, &[]);
    }
}

// -- constants -------------------------------------------------------------------

#[test]
fn i32_const_matches_at_every_signed_leb_boundary() {
    for &value in SIGNED_32_BOUNDARIES {
        let mut code = Vec::new();
        encode::i32_const(&mut code, value);
        let (ours, head, tail) = main_only("i32", code);
        let text = format!("{head} i32.const {value}{tail}");
        let results = agree_running(&format!("i32.const {value}"), &ours, &text, &[]);
        assert_eq!(one_i32(&results), value);
    }
}

#[test]
fn i64_const_matches_at_every_signed_leb_boundary() {
    for &value in SIGNED_64_BOUNDARIES {
        let mut code = Vec::new();
        encode::i64_const(&mut code, value);
        let (ours, head, tail) = main_only("i64", code);
        let text = format!("{head} i64.const {value}{tail}");
        let results = agree_running(&format!("i64.const {value}"), &ours, &text, &[]);
        assert_eq!(one_i64(&results), value);
    }
}

#[test]
fn the_signed_leb_length_steps_exactly_where_it_should() {
    // The differential above says the two encoders agree; this says *what* they
    // agree on, so a shared misunderstanding would still be visible. 63 is one
    // byte and 64 is two; -64 is one byte and -65 is two.
    let length = |value: i64| {
        let mut out = Vec::new();
        encode::signed_64(&mut out, value);
        out.len()
    };
    for step in 1..=9u32 {
        let positive = 1i64 << (7 * step - 1);
        assert_eq!(length(positive - 1), step as usize, "{}", positive - 1);
        assert_eq!(length(positive), step as usize + 1, "{positive}");
        assert_eq!(length(-positive), step as usize, "{}", -positive);
        assert_eq!(
            length(-positive - 1),
            step as usize + 1,
            "{}",
            -positive - 1
        );
    }
    assert_eq!(length(i64::MIN), 10);
    assert_eq!(length(i64::MAX), 10);
}

#[test]
fn the_unsigned_leb_length_steps_exactly_where_it_should() {
    let length = |value: u32| {
        let mut out = Vec::new();
        encode::unsigned(&mut out, value);
        out.len()
    };
    assert_eq!(length(0), 1);
    for step in 1..=4u32 {
        let boundary = 1u32 << (7 * step);
        assert_eq!(length(boundary - 1), step as usize);
        assert_eq!(length(boundary), step as usize + 1);
    }
    assert_eq!(length(u32::MAX), 5);
}

#[test]
fn float_constants_are_raw_bits_in_both_encoders() {
    // Floats are the one immediate that is *not* LEB128, and encoding them
    // through the integer path is a real failure mode. Zero and negative zero,
    // both infinities, a quiet NaN, a NaN with a payload, the subnormal
    // extremes, and a value whose decimal form is not exact.
    let doubles: &[f64] = &[
        0.0,
        -0.0,
        1.0,
        -2.5,
        0.1,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        f64::from_bits(0x7ff0_0000_0000_0001),
        f64::from_bits(0xfff8_0000_0000_0000),
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::MAX,
        f64::MIN,
        9_007_199_254_740_992.0,
    ];
    for &value in doubles {
        let mut code = Vec::new();
        encode::f64_const(&mut code, value);
        let (ours, head, tail) = main_only("f64", code);
        let text = format!("{head} f64.const {}{tail}", wat_f64(value));
        let results = agree_running(&format!("f64.const {value:?}"), &ours, &text, &[]);
        match results.as_slice() {
            [Val::F64(got)] => assert_eq!(
                got.to_bits(),
                value.to_bits(),
                "f64.const {value:?} came back with different bits"
            ),
            other => panic!("expected one f64, got {}", describe(other)),
        }
    }

    let singles: &[f32] = &[
        0.0,
        -0.0,
        1.0,
        -2.5,
        0.1,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
        f32::from_bits(0x7f80_0001),
        f32::MIN_POSITIVE,
        f32::from_bits(1),
        f32::MAX,
        f32::MIN,
    ];
    for &value in singles {
        let mut code = Vec::new();
        encode::f32_const(&mut code, value);
        let (ours, head, tail) = main_only("f32", code);
        let text = format!("{head} f32.const {}{tail}", wat_f32(value));
        let results = agree_running(&format!("f32.const {value:?}"), &ours, &text, &[]);
        match results.as_slice() {
            [Val::F32(got)] => assert_eq!(got.to_bits(), value.to_bits()),
            other => panic!("expected one f32, got {}", describe(other)),
        }
    }
}

// -- variables ---------------------------------------------------------------------

#[test]
fn local_indices_match_at_every_unsigned_leb_boundary() {
    // One function with enough locals to reach index 16384, so `local.get`,
    // `local.set` and `local.tee` are each exercised at a one-, two- and
    // three-byte index. `wat` merges the declaration into one group, so the
    // encoder is handed one group -- see `adjacent_local_groups_stay_unmerged`.
    const INDICES: &[u32] = &[0, 1, 127, 128, 129, 16_383, 16_384];
    let count = INDICES[INDICES.len() - 1] + 1;

    let mut code = Vec::new();
    let mut text_body = String::new();
    for (position, &index) in INDICES.iter().enumerate() {
        let value = position as i32 + 1;
        encode::i32_const(&mut code, value);
        encode::local_set(&mut code, index);
        text_body.push_str(&format!(" i32.const {value} local.set {index}"));
    }
    // `local.tee` leaves its value on the stack, so tee-ing then dropping is
    // the shortest way to reach the opcode without changing the sum below.
    for &index in INDICES {
        encode::local_get(&mut code, index);
        encode::local_tee(&mut code, index);
        encode::op(&mut code, Op::Drop);
        text_body.push_str(&format!(" local.get {index} local.tee {index} drop"));
    }
    for (position, &index) in INDICES.iter().enumerate() {
        encode::local_get(&mut code, index);
        text_body.push_str(&format!(" local.get {index}"));
        if position > 0 {
            encode::op(&mut code, Op::I32Add);
            text_body.push_str(" i32.add");
        }
    }

    let ours = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![FuncBody {
            locals: vec![(count, ValueType::I32)],
            code,
        }],
        ..Builder::default()
    };
    let declaration: String = std::iter::repeat_n("i32 ", count as usize).collect();
    let text = format!(
        "(module (type (func (result i32))) (func (type 0) (local {declaration}){text_body}) \
         (export \"main\" (func 0)))"
    );
    let results = agree_running("local indices", &ours, &text, &[]);
    let expected: i32 = (1..=INDICES.len() as i32).sum();
    assert_eq!(one_i32(&results), expected);
}

#[test]
fn global_indices_match_at_every_unsigned_leb_boundary() {
    // 129 globals is enough to put a `global.get` index on both sides of the
    // one-byte step; the section itself then has a two-byte count.
    const COUNT: u32 = 129;
    const INDICES: &[u32] = &[0, 1, 127, 128];

    let mut code = Vec::new();
    let mut text_body = String::new();
    for (position, &index) in INDICES.iter().enumerate() {
        encode::i32_const(&mut code, position as i32 + 1);
        encode::global_set(&mut code, index);
        text_body.push_str(&format!(" i32.const {} global.set {index}", position + 1));
    }
    for (position, &index) in INDICES.iter().enumerate() {
        encode::global_get(&mut code, index);
        text_body.push_str(&format!(" global.get {index}"));
        if position > 0 {
            encode::op(&mut code, Op::I32Add);
            text_body.push_str(" i32.add");
        }
    }

    let ours = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        globals: (0..COUNT)
            .map(|_| Global {
                ty: ValueType::I32,
                mutable: true,
                init: ConstExpr::I32(0),
            })
            .collect(),
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let declarations: String =
        std::iter::repeat_n("(global (mut i32) (i32.const 0)) ", COUNT as usize).collect();
    let text = format!(
        "(module (type (func (result i32))) {declarations} (export \"main\" (func 0)) \
         (func (type 0){text_body}))"
    );
    let results = agree_running("global indices", &ours, &text, &[]);
    let expected: i32 = (1..=INDICES.len() as i32).sum();
    assert_eq!(one_i32(&results), expected);
}

// -- memory ------------------------------------------------------------------------

/// Every load and store, its wat mnemonic, and the type it needs on the stack
/// under it (a store needs an address and a value; a load needs an address).
const MEM_OPS: &[(MemOp, &str, bool, &str)] = &[
    (MemOp::I32Load, "i32.load", false, "i32"),
    (MemOp::I64Load, "i64.load", false, "i64"),
    (MemOp::F32Load, "f32.load", false, "f32"),
    (MemOp::F64Load, "f64.load", false, "f64"),
    (MemOp::I32Load8S, "i32.load8_s", false, "i32"),
    (MemOp::I32Load8U, "i32.load8_u", false, "i32"),
    (MemOp::I32Load16S, "i32.load16_s", false, "i32"),
    (MemOp::I32Load16U, "i32.load16_u", false, "i32"),
    (MemOp::I64Load8S, "i64.load8_s", false, "i64"),
    (MemOp::I64Load8U, "i64.load8_u", false, "i64"),
    (MemOp::I64Load16S, "i64.load16_s", false, "i64"),
    (MemOp::I64Load16U, "i64.load16_u", false, "i64"),
    (MemOp::I64Load32S, "i64.load32_s", false, "i64"),
    (MemOp::I64Load32U, "i64.load32_u", false, "i64"),
    (MemOp::I32Store, "i32.store", true, "i32"),
    (MemOp::I64Store, "i64.store", true, "i64"),
    (MemOp::F32Store, "f32.store", true, "f32"),
    (MemOp::F64Store, "f64.store", true, "f64"),
    (MemOp::I32Store8, "i32.store8", true, "i32"),
    (MemOp::I32Store16, "i32.store16", true, "i32"),
    (MemOp::I64Store8, "i64.store8", true, "i64"),
    (MemOp::I64Store16, "i64.store16", true, "i64"),
    (MemOp::I64Store32, "i64.store32", true, "i64"),
];

fn push_operand(code: &mut Vec<u8>, ty: &str) {
    match ty {
        "i32" => encode::i32_const(code, 0),
        "i64" => encode::i64_const(code, 0),
        "f32" => encode::f32_const(code, 0.0),
        "f64" => encode::f64_const(code, 0.0),
        other => panic!("unknown operand type {other}"),
    }
}

#[test]
fn the_memory_opcode_table_is_complete() {
    // `MEM_OPS` is a hand-written mirror of the `MemOp` enum, so it can fall
    // behind it silently. The load and store opcodes are the contiguous range
    // 0x28..=0x3e, which is a fact about wasm and not about this table, so
    // checking the range is checking the mirror.
    let mut seen: Vec<u8> = MEM_OPS.iter().map(|(op, ..)| *op as u8).collect();
    seen.sort_unstable();
    seen.dedup();
    let expected: Vec<u8> = (0x28u8..=0x3e).collect();
    assert_eq!(
        seen, expected,
        "MEM_OPS no longer covers every load and store opcode"
    );
}

#[test]
fn every_load_and_store_matches_at_its_natural_alignment() {
    let mut code = Vec::new();
    let mut text_body = String::new();
    for (mem_op, mnemonic, is_store, ty) in MEM_OPS {
        encode::i32_const(&mut code, 0);
        text_body.push_str(" i32.const 0");
        if *is_store {
            push_operand(&mut code, ty);
            text_body.push_str(&format!(" {ty}.const 0"));
        }
        encode::mem(&mut code, *mem_op, 0);
        // An omitted `align=` in the text is the natural alignment, which is
        // exactly the claim `MemOp::natural_align` makes.
        text_body.push_str(&format!(" {mnemonic}"));
        if !*is_store {
            encode::op(&mut code, Op::Drop);
            text_body.push_str(" drop");
        }
    }

    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        memories: vec![Limits { min: 1, max: None }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let text = format!(
        "(module (type (func)) (memory 1) (export \"main\" (func 0)) (func (type 0){text_body}))"
    );
    agree_running("every load and store", &ours, &text, &[]);
}

#[test]
fn reduced_alignment_hints_match_the_reference_assembler() {
    // Below-natural alignment is legal and is different bytes; above-natural is
    // a producer bug the load gate rejects, and `mem_aligned` will not write
    // one. Every exponent from 0 up to each opcode's natural one.
    let mut code = Vec::new();
    let mut text_body = String::new();
    for (mem_op, mnemonic, is_store, ty) in MEM_OPS {
        for exponent in 0..=mem_op.natural_align() {
            encode::i32_const(&mut code, 0);
            text_body.push_str(" i32.const 0");
            if *is_store {
                push_operand(&mut code, ty);
                text_body.push_str(&format!(" {ty}.const 0"));
            }
            encode::mem_aligned(&mut code, *mem_op, exponent, 0);
            // The text spells the alignment in bytes; the bytes spell it as an
            // exponent. Getting that wrong in either direction is a real bug,
            // so the fixture converts rather than tabulating.
            text_body.push_str(&format!(" {mnemonic} align={}", 1u32 << exponent));
            if !*is_store {
                encode::op(&mut code, Op::Drop);
                text_body.push_str(" drop");
            }
        }
    }

    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        memories: vec![Limits { min: 1, max: None }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let text = format!(
        "(module (type (func)) (memory 1) (export \"main\" (func 0)) (func (type 0){text_body}))"
    );
    agree_running("reduced alignment hints", &ours, &text, &[]);
}

#[test]
fn memarg_offsets_match_at_every_unsigned_leb_boundary() {
    // The offset is an unsigned LEB128 with no upper bound below `u32::MAX`, so
    // it reaches five bytes. Nothing here is executed: an offset past the end
    // of a one-page memory traps by definition, and what is under test is the
    // encoding.
    let mut code = Vec::new();
    let mut text_body = String::new();
    for &offset in UNSIGNED_BOUNDARIES {
        encode::i32_const(&mut code, 0);
        encode::mem(&mut code, MemOp::I32Load, offset);
        encode::op(&mut code, Op::Drop);
        text_body.push_str(&format!(" i32.const 0 i32.load offset={offset} drop"));
    }
    let ours = Builder {
        types: vec![sig(&[], &[])],
        functions: vec![0],
        memories: vec![Limits { min: 1, max: None }],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let text = format!("(module (type (func)) (memory 1) (func (type 0){text_body}))");
    agree("memarg offsets", &ours, &text);
}

#[test]
fn memory_size_and_grow_match_and_run() {
    let mut code = Vec::new();
    encode::i32_const(&mut code, 1);
    encode::memory_grow(&mut code, 0);
    encode::op(&mut code, Op::Drop);
    encode::memory_size(&mut code, 0);

    let ours = Builder {
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
    let results = agree_running(
        "memory.size and memory.grow",
        &ours,
        r#"(module
             (type (func (result i32)))
             (memory 1 4)
             (export "main" (func 0))
             (func (type 0) i32.const 1 memory.grow drop memory.size))"#,
        &[],
    );
    assert_eq!(one_i32(&results), 2, "one page grown onto one page");
}

#[test]
fn a_data_segment_is_readable_through_a_load() {
    // The data section and the load opcodes meet here: if either the segment's
    // offset or the memarg's offset were encoded wrong the value would differ
    // even though both modules would still load.
    let mut code = Vec::new();
    encode::i32_const(&mut code, 0);
    encode::mem(&mut code, MemOp::I64Load, 8);

    let ours = Builder {
        types: vec![sig(&[], &[ValueType::I64])],
        functions: vec![0],
        memories: vec![Limits { min: 1, max: None }],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        data: vec![Data::Active {
            memory: 0,
            offset: ConstExpr::I32(8),
            bytes: b"tinyvm-qjs".to_vec(),
        }],
        ..Builder::default()
    };
    let results = agree_running(
        "a data segment read back",
        &ours,
        r#"(module
             (type (func (result i64)))
             (memory 1)
             (export "main" (func 0))
             (data (i32.const 8) "tinyvm-qjs")
             (func (type 0) i32.const 0 i64.load offset=8))"#,
        &[],
    );
    assert_eq!(one_i64(&results), i64::from_le_bytes(*b"tinyvm-q"));
}

// -- control flow --------------------------------------------------------------------

#[test]
fn every_block_type_form_matches() {
    // `Empty` is the byte 0x40, an inline value type is that type's own byte,
    // and a type index is an *s33* -- so index 64 is `c0 00` and not the single
    // byte `40`, which would read back as `Empty`. 63/64 and 8191/8192 are the
    // two places that encoding changes length, and the whole point of the test
    // is that the wrong one of them is still a loadable module.
    const INDICES: &[u32] = &[0, 1, 63, 64, 65, 8_191, 8_192];
    let types = (INDICES[INDICES.len() - 1] + 1) as usize;

    let mut code = Vec::new();
    let mut text_body = String::new();
    for form in [
        BlockType::Empty,
        BlockType::Value(ValueType::I32),
        BlockType::Value(ValueType::I64),
        BlockType::Value(ValueType::F32),
        BlockType::Value(ValueType::F64),
    ] {
        encode::block(&mut code, form);
        match form {
            BlockType::Empty => text_body.push_str(" block"),
            BlockType::Value(ValueType::I32) => {
                encode::i32_const(&mut code, 0);
                text_body.push_str(" block (result i32) i32.const 0");
            }
            BlockType::Value(ValueType::I64) => {
                encode::i64_const(&mut code, 0);
                text_body.push_str(" block (result i64) i64.const 0");
            }
            BlockType::Value(ValueType::F32) => {
                encode::f32_const(&mut code, 0.0);
                text_body.push_str(" block (result f32) f32.const 0");
            }
            BlockType::Value(ValueType::F64) => {
                encode::f64_const(&mut code, 0.0);
                text_body.push_str(" block (result f64) f64.const 0");
            }
            _ => unreachable!(),
        }
        encode::end(&mut code);
        text_body.push_str(" end");
        if !matches!(form, BlockType::Empty) {
            encode::op(&mut code, Op::Drop);
            text_body.push_str(" drop");
        }
    }
    // Every type in the section is `[] -> []`, so a block can name any of them.
    for &index in INDICES {
        encode::block(&mut code, BlockType::TypeIndex(index));
        encode::end(&mut code);
        text_body.push_str(&format!(" block (type {index}) end"));
        encode::loop_(&mut code, BlockType::TypeIndex(index));
        encode::end(&mut code);
        text_body.push_str(&format!(" loop (type {index}) end"));
    }

    let ours = Builder {
        types: (0..types).map(|_| sig(&[], &[])).collect(),
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let declarations: String = std::iter::repeat_n("(type (func)) ", types).collect();
    let text =
        format!("(module {declarations} (export \"main\" (func 0)) (func (type 0){text_body}))");
    let bytes = agree("every block type form", &ours, &text);
    // Say out loud what the s33 boundary looks like, so a future reader does
    // not have to trust the reference assembler to know it was tested.
    assert!(
        windows_contains(&bytes, &[0x02, 0x3f, 0x0b]),
        "block (type 63) should be one byte of block type"
    );
    assert!(
        windows_contains(&bytes, &[0x02, 0xc0, 0x00, 0x0b]),
        "block (type 64) should be the two-byte s33 c0 00, never the byte 40"
    );
    agree_running("every block type form, running", &ours, &text, &[]);
}

fn windows_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn the_unsigned_type_index_and_the_s33_block_type_diverge_at_64() {
    // Type index 64 in the *function* section is the unsigned byte 0x40; the
    // same index as a block type is `c0 00`. One field, two encodings, and the
    // encoder has to know which is which.
    let types = 65usize;
    let mut code = Vec::new();
    encode::block(&mut code, BlockType::TypeIndex(64));
    encode::end(&mut code);

    let ours = Builder {
        types: (0..types).map(|_| sig(&[], &[])).collect(),
        functions: vec![64],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let declarations: String = std::iter::repeat_n("(type (func)) ", types).collect();
    let text = format!("(module {declarations} (func (type 64) block (type 64) end))");
    let bytes = agree("type index 64 in two fields", &ours, &text);
    assert!(
        windows_contains(&bytes, &[encode::SECTION_FUNCTION, 0x02, 0x01, 0x40]),
        "the function section should carry the plain unsigned byte 0x40"
    );
    assert!(
        windows_contains(&bytes, &[0x02, 0xc0, 0x00, 0x0b]),
        "the block type should carry the s33 c0 00"
    );
}

#[test]
fn if_else_and_end_match_and_run() {
    let mut code = Vec::new();
    encode::local_get(&mut code, 0);
    encode::if_(&mut code, BlockType::Value(ValueType::I32));
    encode::i32_const(&mut code, 10);
    encode::else_(&mut code);
    encode::i32_const(&mut code, 20);
    encode::end(&mut code);
    // An `if` with no `else` is a different byte string, and its empty block
    // type is the one that reads as 0x40.
    encode::local_get(&mut code, 0);
    encode::if_(&mut code, BlockType::Empty);
    encode::op(&mut code, Op::Nop);
    encode::end(&mut code);

    let ours = Builder {
        types: vec![sig(&[ValueType::I32], &[ValueType::I32])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let text = r#"(module
                    (type (func (param i32) (result i32)))
                    (export "main" (func 0))
                    (func (type 0)
                      local.get 0
                      if (result i32) i32.const 10 else i32.const 20 end
                      local.get 0
                      if nop end))"#;
    assert_eq!(
        one_i32(&agree_running("if/else", &ours, text, &[Val::I32(1)])),
        10
    );
    assert_eq!(
        one_i32(&agree_running("if/else", &ours, text, &[Val::I32(0)])),
        20
    );
}

#[test]
fn branch_depths_match_at_every_unsigned_leb_boundary() {
    // A branch depth is an unsigned LEB128, so reaching a two-byte one means
    // nesting 129 blocks. The branch is taken from the innermost block to the
    // outermost, which is the only depth worth spending 129 blocks on.
    for depth in [0u32, 1, 126, 127, 128, 129] {
        let mut code = Vec::new();
        let mut text_body = String::new();
        for _ in 0..=depth {
            encode::block(&mut code, BlockType::Empty);
            text_body.push_str(" block");
        }
        encode::br(&mut code, depth);
        text_body.push_str(&format!(" br {depth}"));
        for _ in 0..=depth {
            encode::end(&mut code);
            text_body.push_str(" end");
        }
        // ...and the same depth reached conditionally, which is a different
        // opcode with the same immediate.
        for _ in 0..=depth {
            encode::block(&mut code, BlockType::Empty);
            text_body.push_str(" block");
        }
        encode::i32_const(&mut code, 1);
        encode::br_if(&mut code, depth);
        text_body.push_str(&format!(" i32.const 1 br_if {depth}"));
        for _ in 0..=depth {
            encode::end(&mut code);
            text_body.push_str(" end");
        }
        encode::i32_const(&mut code, depth as i32);

        let (ours, head, tail) = main_only("i32", code);
        let text = format!("{head}{text_body} i32.const {depth}{tail}");
        let results = agree_running(&format!("br {depth}"), &ours, &text, &[]);
        assert_eq!(one_i32(&results), depth as i32);
    }
}

#[test]
fn br_table_matches_over_its_target_vector() {
    // The default label sits *outside* the vector and is not counted by it.
    // A zero-length vector and a vector whose count needs two bytes are the two
    // ends of getting that wrong.
    for targets in [0usize, 1, 2, 127, 128] {
        let mut code = Vec::new();
        let mut text_body = String::new();
        encode::block(&mut code, BlockType::Empty);
        text_body.push_str(" block");
        encode::i32_const(&mut code, 0);
        text_body.push_str(" i32.const 0");
        let labels: Vec<u32> = vec![0; targets];
        encode::br_table(&mut code, &labels, 0);
        text_body.push_str(" br_table");
        for _ in 0..targets {
            text_body.push_str(" 0");
        }
        text_body.push_str(" 0");
        encode::end(&mut code);
        text_body.push_str(" end");
        encode::i32_const(&mut code, targets as i32);

        let (ours, head, tail) = main_only("i32", code);
        let text = format!("{head}{text_body} i32.const {targets}{tail}");
        let results = agree_running(
            &format!("br_table with {targets} targets"),
            &ours,
            &text,
            &[],
        );
        assert_eq!(one_i32(&results), targets as i32);
    }
}

#[test]
fn a_loop_that_counts_matches_and_runs() {
    // `loop` branches backwards, which no other fixture here does, and the
    // count makes the branch's arithmetic observable.
    let mut code = Vec::new();
    encode::i32_const(&mut code, 0);
    encode::local_set(&mut code, 0);
    encode::block(&mut code, BlockType::Empty);
    encode::loop_(&mut code, BlockType::Empty);
    encode::local_get(&mut code, 0);
    encode::i32_const(&mut code, 1);
    encode::op(&mut code, Op::I32Add);
    encode::local_tee(&mut code, 0);
    encode::i32_const(&mut code, 10);
    encode::op(&mut code, Op::I32LtS);
    encode::br_if(&mut code, 0);
    encode::end(&mut code);
    encode::end(&mut code);
    encode::local_get(&mut code, 0);

    let ours = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![FuncBody {
            locals: vec![(1, ValueType::I32)],
            code,
        }],
        ..Builder::default()
    };
    let results = agree_running(
        "a counting loop",
        &ours,
        r#"(module
             (type (func (result i32)))
             (export "main" (func 0))
             (func (type 0) (local i32)
               i32.const 0
               local.set 0
               block
                 loop
                   local.get 0
                   i32.const 1
                   i32.add
                   local.tee 0
                   i32.const 10
                   i32.lt_s
                   br_if 0
                 end
               end
               local.get 0))"#,
        &[],
    );
    assert_eq!(one_i32(&results), 10);
}

#[test]
fn call_indices_match_at_every_unsigned_leb_boundary() {
    // 200 functions puts a callee index on both sides of the one-byte step, and
    // makes the function and code sections carry a two-byte count.
    const COUNT: u32 = 200;
    const CALLEES: &[u32] = &[0, 1, 127, 128, 199];

    let mut declarations = String::new();
    let mut bodies = Vec::new();
    for index in 0..COUNT {
        let mut leaf = Vec::new();
        encode::i32_const(&mut leaf, index as i32);
        bodies.push(body(leaf));
        declarations.push_str(&format!("(func (type 0) i32.const {index}) "));
    }

    // Function `COUNT` is `main`: it calls each of the chosen indices and sums
    // what they return, so a misencoded index is a wrong number rather than a
    // module that merely still loads.
    let mut code = Vec::new();
    let mut text_body = String::new();
    for (position, &callee) in CALLEES.iter().enumerate() {
        encode::call(&mut code, callee);
        text_body.push_str(&format!(" call {callee}"));
        if position > 0 {
            encode::op(&mut code, Op::I32Add);
            text_body.push_str(" i32.add");
        }
    }
    bodies.push(body(code));

    let ours = Builder {
        types: vec![sig(&[], &[ValueType::I32])],
        functions: (0..=COUNT).map(|_| 0).collect(),
        exports: vec![export("main", DESCRIPTOR_FUNC, COUNT)],
        bodies,
        ..Builder::default()
    };
    let text = format!(
        "(module (type (func (result i32))) (export \"main\" (func {COUNT})) \
         {declarations}(func (type 0){text_body}))"
    );
    let results = agree_running("call indices", &ours, &text, &[]);
    assert_eq!(one_i32(&results), CALLEES.iter().sum::<u32>() as i32);
}

#[test]
fn call_indirect_matches_over_its_type_and_table_indices() {
    // `call_indirect` names a type and then a table, in that order, and both
    // are unsigned LEB128. Two tables and 129 types put each of them on both
    // sides of the one-byte step.
    const TYPES: u32 = 129;

    let mut code = Vec::new();
    let mut text_body = String::new();
    // Table 0 holds function 0, table 1 holds function 1. Calling through each
    // with a type index on either side of the step and summing the results
    // makes every one of the four immediates observable.
    for (table, type_index) in [(0u32, 0u32), (1, 128)] {
        encode::i32_const(&mut code, 0);
        encode::call_indirect(&mut code, type_index, table);
        text_body.push_str(&format!(
            " i32.const 0 call_indirect {table} (type {type_index})"
        ));
    }
    encode::op(&mut code, Op::I32Add);
    text_body.push_str(" i32.add");

    let mut bodies = vec![body(Vec::new()), body(Vec::new())];
    let mut leaf0 = Vec::new();
    encode::i32_const(&mut leaf0, 3);
    bodies[0] = body(leaf0);
    let mut leaf1 = Vec::new();
    encode::i32_const(&mut leaf1, 4);
    bodies[1] = body(leaf1);
    bodies.push(body(code));

    // Every type in the section is `[] -> [i32]`, so index 0 and index 128 name
    // the same signature and only their encodings differ.
    let ours = Builder {
        types: (0..TYPES).map(|_| sig(&[], &[ValueType::I32])).collect(),
        functions: vec![0, 0, 0],
        tables: vec![
            TableType {
                element: ValueType::FuncRef,
                limits: Limits {
                    min: 1,
                    max: Some(1),
                },
            },
            TableType {
                element: ValueType::FuncRef,
                limits: Limits {
                    min: 1,
                    max: Some(1),
                },
            },
        ],
        exports: vec![export("main", DESCRIPTOR_FUNC, 2)],
        elements: vec![
            Element::ActiveFuncs {
                table: 0,
                offset: ConstExpr::I32(0),
                funcs: vec![0],
            },
            Element::ActiveFuncs {
                table: 1,
                offset: ConstExpr::I32(0),
                funcs: vec![1],
            },
        ],
        bodies,
        ..Builder::default()
    };
    let declarations: String =
        std::iter::repeat_n("(type (func (result i32))) ", TYPES as usize).collect();
    let text = format!(
        "(module {declarations}(table 1 1 funcref) (table 1 1 funcref) \
         (export \"main\" (func 2)) (elem (i32.const 0) 0) (elem (table 1) (i32.const 0) func 1) \
         (func (type 0) i32.const 3) (func (type 0) i32.const 4) (func (type 0){text_body}))"
    );
    let results = agree_running("call_indirect indices", &ours, &text, &[]);
    assert_eq!(one_i32(&results), 7);
}

#[test]
fn return_and_unreachable_match_and_run() {
    let mut code = Vec::new();
    encode::i32_const(&mut code, 7);
    encode::op(&mut code, Op::Return);
    encode::op(&mut code, Op::Unreachable);

    let (ours, head, tail) = main_only("i32", code);
    let text = format!("{head} i32.const 7 return unreachable{tail}");
    let results = agree_running("return before unreachable", &ours, &text, &[]);
    assert_eq!(one_i32(&results), 7);
}

#[test]
fn select_and_drop_match_and_run() {
    let mut code = Vec::new();
    encode::i32_const(&mut code, 11);
    encode::i32_const(&mut code, 22);
    encode::local_get(&mut code, 0);
    encode::op(&mut code, Op::Select);

    let ours = Builder {
        types: vec![sig(&[ValueType::I32], &[ValueType::I32])],
        functions: vec![0],
        exports: vec![export("main", DESCRIPTOR_FUNC, 0)],
        bodies: vec![body(code)],
        ..Builder::default()
    };
    let text = r#"(module
                    (type (func (param i32) (result i32)))
                    (export "main" (func 0))
                    (func (type 0) i32.const 11 i32.const 22 local.get 0 select))"#;
    assert_eq!(
        one_i32(&agree_running("select", &ours, text, &[Val::I32(1)])),
        11
    );
    assert_eq!(
        one_i32(&agree_running("select", &ours, text, &[Val::I32(0)])),
        22
    );
}
