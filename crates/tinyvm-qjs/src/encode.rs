//! [`super::ir::Module`] -> standard `.wasm` bytes.
//!
//! Written by hand, and that is a requirement rather than an accident: the
//! output has to clear tinyvm's load gate, which is strict about the things a
//! general-purpose encoder crate hides -- canonical section order, minimal
//! LEB128, the exact `end` that terminates a function expression, memarg
//! alignment, signed-LEB range. We own that correctness because the product
//! depends on it; a dependency would only let us assume it.
//!
//! The encoder is total. Anything it could reject, [`super::parse`] already
//! rejected with a diagnostic that names a capability boundary, so nothing here
//! returns a `Result` and no failure reaches the user as bytes.
//!
//! # Wider than today's lowering
//!
//! [`encode`] emits what [`super::emit`] currently builds, which is integer
//! expressions in one function. Everything else below -- the memory, global,
//! table, start, element, data-count and data sections, the `i64`/`f64`/control
//! opcode families, `call_indirect`, the memarg forms -- is the surface the
//! next lowering milestones consume, and it is here now because *encoding* is
//! where the load gate's strictness lives and where it can be measured against
//! a reference assembler. Each capability is locked by `tests/encode_sections.rs`,
//! which is where this module's tests live: the evidence that matters is a
//! module going through `WasmModule::from_bytes_with` and coming out byte-equal
//! to `wat`, and neither of those belongs in a unit test inside `src`.

// Most of what follows is reachable from the tests but not yet from `encode`,
// because `ir` cannot name a memory or an `i64` yet. Blanket-allowing the
// warning is the honest form of that: the alternative is twenty-odd individual
// attributes saying the same sentence, and the lint would go quiet again the
// moment the first lowering milestone lands.
#![allow(dead_code)]

use super::ir::{ExportKind, Func, FuncType, Import, Ins, Module, ValType};

/// The import-descriptor byte for a function, and the export-descriptor byte
/// for one. They are the same value in two different tables.
pub(crate) const DESCRIPTOR_FUNC: u8 = 0x00;
pub(crate) const DESCRIPTOR_TABLE: u8 = 0x01;
pub(crate) const DESCRIPTOR_MEMORY: u8 = 0x02;
pub(crate) const DESCRIPTOR_GLOBAL: u8 = 0x03;

/// `\0asm` plus version 1, little-endian. The eight bytes every module starts
/// with.
pub(crate) const HEADER: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

// Section ids. Numeric order is *not* the required order: the data-count
// section is id 12 but sits between element (9) and code (10). tinyvm ranks
// them exactly that way (`crates/tinyvm/src/wasm.rs`, `from_bytes_with`), so
// `encode` emits them in the order below and not in id order.
pub(crate) const SECTION_TYPE: u8 = 1;
pub(crate) const SECTION_IMPORT: u8 = 2;
pub(crate) const SECTION_FUNCTION: u8 = 3;
pub(crate) const SECTION_TABLE: u8 = 4;
pub(crate) const SECTION_MEMORY: u8 = 5;
pub(crate) const SECTION_GLOBAL: u8 = 6;
pub(crate) const SECTION_EXPORT: u8 = 7;
pub(crate) const SECTION_START: u8 = 8;
pub(crate) const SECTION_ELEMENT: u8 = 9;
pub(crate) const SECTION_CODE: u8 = 10;
pub(crate) const SECTION_DATA: u8 = 11;
pub(crate) const SECTION_DATA_COUNT: u8 = 12;
pub(crate) const SECTION_CUSTOM: u8 = 0;

/// Marks the start of a function type in the type section.
const FUNC_TYPE_TAG: u8 = 0x60;
/// Terminates an expression -- the `end` opcode.
const END: u8 = 0x0b;

/// The one entry point the compiler uses. Everything it does is done through
/// the same section writers a future lowering will call directly, so there is
/// no privileged path: [`super::ir`] is translated into the encoder's own
/// vocabulary here and nowhere else.
pub(crate) fn encode(module: &Module) -> Vec<u8> {
    let types: Vec<Signature> = module.types.iter().map(Signature::from).collect();
    let type_indices: Vec<u32> = module.funcs.iter().map(|f| f.type_index).collect();
    let exports: Vec<ExportEntry> = module
        .exports
        .iter()
        .map(|e| ExportEntry {
            name: e.name.clone(),
            descriptor: match e.kind {
                ExportKind::Func => DESCRIPTOR_FUNC,
            },
            index: e.index,
        })
        .collect();
    let bodies: Vec<FuncBody> = module.funcs.iter().map(FuncBody::from).collect();
    let imports: Vec<ImportEntry> = module.imports.iter().map(ImportEntry::from).collect();

    let mut out = HEADER.to_vec();
    type_section(&mut out, &types);
    // Only when there is something to import. An empty import section is legal
    // and means nothing, and a module that imports nothing should not carry a
    // section saying so.
    if !imports.is_empty() {
        import_section(&mut out, &imports);
    }
    function_section(&mut out, &type_indices);
    export_section(&mut out, &exports);
    code_section(&mut out, &bodies);
    out
}

// -- sections ----------------------------------------------------------------

pub(crate) fn type_section(out: &mut Vec<u8>, types: &[Signature]) {
    section(out, SECTION_TYPE, |body| {
        vector(body, types, |body, ty| {
            body.push(FUNC_TYPE_TAG);
            vector(body, &ty.params, |body, t| body.push(t.byte()));
            vector(body, &ty.results, |body, t| body.push(t.byte()));
        });
    });
}

/// The import section. An imported entity occupies the *low* indices of its
/// index space, ahead of every defined one, which is why the order here is part
/// of the module's meaning and not a presentation choice.
pub(crate) fn import_section(out: &mut Vec<u8>, imports: &[ImportEntry]) {
    section(out, SECTION_IMPORT, |body| {
        vector(body, imports, |body, import| {
            name(body, &import.module);
            name(body, &import.name);
            match &import.desc {
                ImportDesc::Func(type_index) => {
                    body.push(DESCRIPTOR_FUNC);
                    unsigned(body, *type_index);
                }
                ImportDesc::Table(table) => {
                    body.push(DESCRIPTOR_TABLE);
                    body.push(table.element.byte());
                    limits(body, table.limits);
                }
                ImportDesc::Memory(memory) => {
                    body.push(DESCRIPTOR_MEMORY);
                    limits(body, *memory);
                }
                ImportDesc::Global { ty, mutable } => {
                    body.push(DESCRIPTOR_GLOBAL);
                    body.push(ty.byte());
                    body.push(u8::from(*mutable));
                }
            }
        });
    });
}

/// The function section: one type index per *defined* function, in the order
/// their bodies appear in the code section. tinyvm rejects a module whose two
/// counts disagree.
pub(crate) fn function_section(out: &mut Vec<u8>, type_indices: &[u32]) {
    section(out, SECTION_FUNCTION, |body| {
        vector(body, type_indices, |body, index| unsigned(body, *index));
    });
}

/// The table section. A table is a reference type plus limits; tinyvm accepts
/// `funcref` and `externref` and nothing else (`parse_table_section`).
pub(crate) fn table_section(out: &mut Vec<u8>, tables: &[TableType]) {
    section(out, SECTION_TABLE, |body| {
        vector(body, tables, |body, table| {
            body.push(table.element.byte());
            limits(body, table.limits);
        });
    });
}

/// The memory section: just a vector of limits, since a memory has no other
/// type structure in this version of wasm.
pub(crate) fn memory_section(out: &mut Vec<u8>, memories: &[Limits]) {
    section(out, SECTION_MEMORY, |body| {
        vector(body, memories, |body, m| limits(body, *m));
    });
}

pub(crate) fn global_section(out: &mut Vec<u8>, globals: &[Global]) {
    section(out, SECTION_GLOBAL, |body| {
        vector(body, globals, |body, global| {
            body.push(global.ty.byte());
            body.push(u8::from(global.mutable));
            const_expr(body, &global.init);
        });
    });
}

pub(crate) fn export_section(out: &mut Vec<u8>, exports: &[ExportEntry]) {
    section(out, SECTION_EXPORT, |body| {
        vector(body, exports, |body, export| {
            name(body, &export.name);
            body.push(export.descriptor);
            unsigned(body, export.index);
        });
    });
}

/// The start section holds one function index and nothing else. tinyvm rejects
/// any trailing byte here, and requires the named function to have type
/// `[] -> []`.
pub(crate) fn start_section(out: &mut Vec<u8>, func: u32) {
    section(out, SECTION_START, |body| unsigned(body, func));
}

pub(crate) fn element_section(out: &mut Vec<u8>, elements: &[Element]) {
    section(out, SECTION_ELEMENT, |body| {
        vector(body, elements, |body, element| match element {
            // Flag 0 is the one form a `call_indirect` producer needs: active,
            // table 0, function indices. Its offset expression must be `i32`.
            Element::ActiveFuncs {
                table: 0,
                offset,
                funcs,
            } => {
                unsigned(body, 0);
                const_expr(body, offset);
                vector(body, funcs, |body, f| unsigned(body, *f));
            }
            // Flag 2 spells the table index out, and carries an element-kind
            // byte (0 = funcref) that flag 0 leaves implicit.
            Element::ActiveFuncs {
                table,
                offset,
                funcs,
            } => {
                unsigned(body, 2);
                unsigned(body, *table);
                const_expr(body, offset);
                body.push(0x00);
                vector(body, funcs, |body, f| unsigned(body, *f));
            }
            Element::PassiveFuncs(funcs) => {
                unsigned(body, 1);
                body.push(0x00);
                vector(body, funcs, |body, f| unsigned(body, *f));
            }
        });
    });
}

/// The data-count section. Required ahead of the code section by any module
/// whose code uses `memory.init` or `data.drop`; tinyvm checks the count
/// against the data section's length and rejects a mismatch.
pub(crate) fn data_count_section(out: &mut Vec<u8>, count: u32) {
    section(out, SECTION_DATA_COUNT, |body| unsigned(body, count));
}

pub(crate) fn code_section(out: &mut Vec<u8>, bodies: &[FuncBody]) {
    section(out, SECTION_CODE, |body| {
        vector(body, bodies, |body, func| {
            // Each entry is size-prefixed, and the size covers the locals
            // declaration plus the expression. Build it, then measure it.
            let mut code = Vec::new();
            locals(&mut code, &func.locals);
            code.extend_from_slice(&func.code);
            code.push(END);
            unsigned(body, code.len() as u32);
            body.extend_from_slice(&code);
        });
    });
}

pub(crate) fn data_section(out: &mut Vec<u8>, segments: &[Data]) {
    section(out, SECTION_DATA, |body| {
        vector(body, segments, |body, segment| {
            match segment {
                Data::Active {
                    memory: 0, offset, ..
                } => {
                    unsigned(body, 0);
                    const_expr(body, offset);
                }
                Data::Active { memory, offset, .. } => {
                    unsigned(body, 2);
                    unsigned(body, *memory);
                    const_expr(body, offset);
                }
                Data::Passive(_) => unsigned(body, 1),
            }
            let bytes = segment.bytes();
            unsigned(body, bytes.len() as u32);
            body.extend_from_slice(bytes);
        });
    });
}

/// A custom section: a name, then opaque bytes. tinyvm reads the name and
/// requires it to be valid UTF-8 with a length that stays inside the section,
/// so the name is encoded through the same [`name`] the import table uses.
pub(crate) fn custom_section(out: &mut Vec<u8>, section_name: &str, contents: &[u8]) {
    section(out, SECTION_CUSTOM, |body| {
        name(body, section_name);
        body.extend_from_slice(contents);
    });
}

/// A section: its id, its byte length, then its contents. The length is
/// measured rather than predicted, which is why the body is built first.
pub(crate) fn section(out: &mut Vec<u8>, id: u8, build: impl FnOnce(&mut Vec<u8>)) {
    let mut body = Vec::new();
    build(&mut body);
    out.push(id);
    unsigned(out, body.len() as u32);
    out.extend_from_slice(&body);
}

/// A wasm vector: an element count, then the elements.
pub(crate) fn vector<T>(out: &mut Vec<u8>, items: &[T], mut element: impl FnMut(&mut Vec<u8>, &T)) {
    unsigned(out, items.len() as u32);
    for item in items {
        element(out, item);
    }
}

/// A name: its byte length, then its UTF-8 bytes. Rust `str` is already valid
/// UTF-8, which is exactly what the load gate checks for.
pub(crate) fn name(out: &mut Vec<u8>, text: &str) {
    unsigned(out, text.len() as u32);
    out.extend_from_slice(text.as_bytes());
}

/// Limits: a flag saying whether a maximum follows, then the minimum, then the
/// maximum if there is one. Emitting flag `0x01` with a maximum equal to the
/// minimum is legal but is not the same bytes as flag `0x00`, so the two cases
/// stay distinct rather than being normalised.
pub(crate) fn limits(out: &mut Vec<u8>, limits: Limits) {
    match limits.max {
        None => {
            out.push(0x00);
            unsigned(out, limits.min);
        }
        Some(max) => {
            out.push(0x01);
            unsigned(out, limits.min);
            unsigned(out, max);
        }
    }
}

/// The locals declaration: run-length `(count, type)` groups.
///
/// Adjacent groups of the same type are *not* merged here -- a caller that
/// wants `(local i64 i64)` as one group builds one group, and one that wants
/// two groups gets two, because the two are different bytes and only the caller
/// knows which it meant. What the encoder does guarantee is the shape tinyvm's
/// `parse_code_section` reads: a vector, then the expression, with no separator
/// between them. Parameters are *not* declared here; they are already locals
/// `0..n` by virtue of the function's type, and repeating them would shift
/// every `local.get` index by `n`.
pub(crate) fn locals(out: &mut Vec<u8>, groups: &[(u32, ValueType)]) {
    vector(out, groups, |out, (count, ty)| {
        unsigned(out, *count);
        out.push(ty.byte());
    });
}

// -- types the `ir` vocabulary does not carry yet -----------------------------

/// A wasm value type. [`super::ir::ValType`] is the subset today's lowering can
/// name; this is the full set the encoder can write, and the `From` impl below
/// is the one place the two meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueType {
    I32,
    I64,
    F32,
    F64,
    FuncRef,
    ExternRef,
}

impl ValueType {
    pub(crate) fn byte(self) -> u8 {
        match self {
            ValueType::I32 => 0x7f,
            ValueType::I64 => 0x7e,
            ValueType::F32 => 0x7d,
            ValueType::F64 => 0x7c,
            ValueType::FuncRef => 0x70,
            ValueType::ExternRef => 0x6f,
        }
    }
}

impl From<ValType> for ValueType {
    fn from(ty: ValType) -> Self {
        match ty {
            ValType::I32 => ValueType::I32,
        }
    }
}

/// A function signature over the encoder's own value types.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Signature {
    pub(crate) params: Vec<ValueType>,
    pub(crate) results: Vec<ValueType>,
}

impl From<&FuncType> for Signature {
    fn from(ty: &FuncType) -> Self {
        Signature {
            params: ty.params.iter().copied().map(ValueType::from).collect(),
            results: ty.results.iter().copied().map(ValueType::from).collect(),
        }
    }
}

/// One code-section entry: the locals beyond the parameters, and the already
/// encoded instruction bytes.
///
/// `code` holds *bytes*, not instructions, because that is what makes the
/// instruction writers below a usable surface: a lowering pass appends to a
/// `Vec<u8>` as it walks its tree and hands the result over. The terminating
/// `end` is not part of it -- [`code_section`] appends that.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct FuncBody {
    pub(crate) locals: Vec<(u32, ValueType)>,
    pub(crate) code: Vec<u8>,
}

impl From<&Func> for FuncBody {
    fn from(func: &Func) -> Self {
        let mut code = Vec::new();
        for ins in &func.body {
            instruction(&mut code, ins);
        }
        FuncBody {
            locals: func
                .locals
                .iter()
                .map(|(count, ty)| (*count, ValueType::from(*ty)))
                .collect(),
            code,
        }
    }
}

/// One export-section entry. The descriptor is one of the `DESCRIPTOR_*`
/// bytes, so a memory or global export needs no new type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportEntry {
    pub(crate) name: String,
    pub(crate) descriptor: u8,
    pub(crate) index: u32,
}

/// What an import brings in. An export is an index into an already-built space
/// so one byte plus a number says everything; an import *declares* the thing,
/// so it carries the type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportDesc {
    Func(u32),
    Table(TableType),
    Memory(Limits),
    Global { ty: ValueType, mutable: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportEntry {
    pub(crate) module: String,
    pub(crate) name: String,
    pub(crate) desc: ImportDesc,
}

impl From<&Import> for ImportEntry {
    fn from(import: &Import) -> Self {
        ImportEntry {
            module: import.module.clone(),
            name: import.name.clone(),
            desc: ImportDesc::Func(import.type_index),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) min: u32,
    pub(crate) max: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TableType {
    pub(crate) element: ValueType,
    pub(crate) limits: Limits,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Global {
    pub(crate) ty: ValueType,
    pub(crate) mutable: bool,
    pub(crate) init: ConstExpr,
}

/// A constant expression: one instruction, then `end`.
///
/// wasm allows more than one instruction here; tinyvm allows more still (it
/// folds `i32.add` and friends). One is what a producer needs and one is what
/// stays obviously canonical, so that is what this encodes.
///
/// [`ConstExpr::GlobalGet`] has a load-gate constraint that is easy to miss:
/// tinyvm resolves the index against the *imported* globals only, and rejects
/// a mutable one (`parse_const_expr`). A defined global cannot initialise
/// another defined global.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConstExpr {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    GlobalGet(u32),
    RefNull(ValueType),
    RefFunc(u32),
}

pub(crate) fn const_expr(out: &mut Vec<u8>, expr: &ConstExpr) {
    match *expr {
        ConstExpr::I32(v) => i32_const(out, v),
        ConstExpr::I64(v) => i64_const(out, v),
        ConstExpr::F32(v) => f32_const(out, v),
        ConstExpr::F64(v) => f64_const(out, v),
        ConstExpr::GlobalGet(index) => global_get(out, index),
        ConstExpr::RefNull(ty) => {
            out.push(0xd0);
            out.push(ty.byte());
        }
        ConstExpr::RefFunc(index) => {
            out.push(0xd2);
            unsigned(out, index);
        }
    }
    out.push(END);
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Element {
    ActiveFuncs {
        table: u32,
        offset: ConstExpr,
        funcs: Vec<u32>,
    },
    PassiveFuncs(Vec<u32>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Data {
    Active {
        memory: u32,
        offset: ConstExpr,
        bytes: Vec<u8>,
    },
    Passive(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Data::Active { bytes, .. } | Data::Passive(bytes) => bytes,
        }
    }
}

// -- instructions -------------------------------------------------------------

fn instruction(out: &mut Vec<u8>, ins: &Ins) {
    match ins {
        Ins::I32Const(value) => i32_const(out, *value),
        Ins::LocalGet(index) => local_get(out, *index),
        Ins::Call(index) => call(out, *index),
        Ins::I32Add => op(out, Op::I32Add),
        Ins::I32Sub => op(out, Op::I32Sub),
        Ins::I32Mul => op(out, Op::I32Mul),
        Ins::I32DivS => op(out, Op::I32DivS),
        Ins::I32RemS => op(out, Op::I32RemS),
    }
}

/// Every wasm instruction that is one opcode byte and no immediate. The
/// discriminant *is* the opcode, so this enum is the opcode table rather than
/// a thing that needs one, and `tests/encode_sections.rs` checks the whole
/// table against a reference assembler in one pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Op {
    Unreachable = 0x00,
    Nop = 0x01,
    Return = 0x0f,
    Drop = 0x1a,
    Select = 0x1b,

    I32Eqz = 0x45,
    I32Eq = 0x46,
    I32Ne = 0x47,
    I32LtS = 0x48,
    I32LtU = 0x49,
    I32GtS = 0x4a,
    I32GtU = 0x4b,
    I32LeS = 0x4c,
    I32LeU = 0x4d,
    I32GeS = 0x4e,
    I32GeU = 0x4f,

    I64Eqz = 0x50,
    I64Eq = 0x51,
    I64Ne = 0x52,
    I64LtS = 0x53,
    I64LtU = 0x54,
    I64GtS = 0x55,
    I64GtU = 0x56,
    I64LeS = 0x57,
    I64LeU = 0x58,
    I64GeS = 0x59,
    I64GeU = 0x5a,

    F32Eq = 0x5b,
    F32Ne = 0x5c,
    F32Lt = 0x5d,
    F32Gt = 0x5e,
    F32Le = 0x5f,
    F32Ge = 0x60,

    F64Eq = 0x61,
    F64Ne = 0x62,
    F64Lt = 0x63,
    F64Gt = 0x64,
    F64Le = 0x65,
    F64Ge = 0x66,

    I32Clz = 0x67,
    I32Ctz = 0x68,
    I32Popcnt = 0x69,
    I32Add = 0x6a,
    I32Sub = 0x6b,
    I32Mul = 0x6c,
    I32DivS = 0x6d,
    I32DivU = 0x6e,
    I32RemS = 0x6f,
    I32RemU = 0x70,
    I32And = 0x71,
    I32Or = 0x72,
    I32Xor = 0x73,
    I32Shl = 0x74,
    I32ShrS = 0x75,
    I32ShrU = 0x76,
    I32Rotl = 0x77,
    I32Rotr = 0x78,

    I64Clz = 0x79,
    I64Ctz = 0x7a,
    I64Popcnt = 0x7b,
    I64Add = 0x7c,
    I64Sub = 0x7d,
    I64Mul = 0x7e,
    I64DivS = 0x7f,
    I64DivU = 0x80,
    I64RemS = 0x81,
    I64RemU = 0x82,
    I64And = 0x83,
    I64Or = 0x84,
    I64Xor = 0x85,
    I64Shl = 0x86,
    I64ShrS = 0x87,
    I64ShrU = 0x88,
    I64Rotl = 0x89,
    I64Rotr = 0x8a,

    F32Abs = 0x8b,
    F32Neg = 0x8c,
    F32Ceil = 0x8d,
    F32Floor = 0x8e,
    F32Trunc = 0x8f,
    F32Nearest = 0x90,
    F32Sqrt = 0x91,
    F32Add = 0x92,
    F32Sub = 0x93,
    F32Mul = 0x94,
    F32Div = 0x95,
    F32Min = 0x96,
    F32Max = 0x97,
    F32Copysign = 0x98,

    F64Abs = 0x99,
    F64Neg = 0x9a,
    F64Ceil = 0x9b,
    F64Floor = 0x9c,
    F64Trunc = 0x9d,
    F64Nearest = 0x9e,
    F64Sqrt = 0x9f,
    F64Add = 0xa0,
    F64Sub = 0xa1,
    F64Mul = 0xa2,
    F64Div = 0xa3,
    F64Min = 0xa4,
    F64Max = 0xa5,
    F64Copysign = 0xa6,

    I32WrapI64 = 0xa7,
    I32TruncF32S = 0xa8,
    I32TruncF32U = 0xa9,
    I32TruncF64S = 0xaa,
    I32TruncF64U = 0xab,
    I64ExtendI32S = 0xac,
    I64ExtendI32U = 0xad,
    I64TruncF32S = 0xae,
    I64TruncF32U = 0xaf,
    I64TruncF64S = 0xb0,
    I64TruncF64U = 0xb1,
    F32ConvertI32S = 0xb2,
    F32ConvertI32U = 0xb3,
    F32ConvertI64S = 0xb4,
    F32ConvertI64U = 0xb5,
    F32DemoteF64 = 0xb6,
    F64ConvertI32S = 0xb7,
    F64ConvertI32U = 0xb8,
    F64ConvertI64S = 0xb9,
    F64ConvertI64U = 0xba,
    F64PromoteF32 = 0xbb,
    I32ReinterpretF32 = 0xbc,
    I64ReinterpretF64 = 0xbd,
    F32ReinterpretI32 = 0xbe,
    F64ReinterpretI64 = 0xbf,

    I32Extend8S = 0xc0,
    I32Extend16S = 0xc1,
    I64Extend8S = 0xc2,
    I64Extend16S = 0xc3,
    I64Extend32S = 0xc4,
}

pub(crate) fn op(out: &mut Vec<u8>, op: Op) {
    out.push(op as u8);
}

/// Every load and store. The discriminant is the opcode; [`MemOp::natural_align`]
/// is the alignment exponent that opcode's width implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum MemOp {
    I32Load = 0x28,
    I64Load = 0x29,
    F32Load = 0x2a,
    F64Load = 0x2b,
    I32Load8S = 0x2c,
    I32Load8U = 0x2d,
    I32Load16S = 0x2e,
    I32Load16U = 0x2f,
    I64Load8S = 0x30,
    I64Load8U = 0x31,
    I64Load16S = 0x32,
    I64Load16U = 0x33,
    I64Load32S = 0x34,
    I64Load32U = 0x35,
    I32Store = 0x36,
    I64Store = 0x37,
    F32Store = 0x38,
    F64Store = 0x39,
    I32Store8 = 0x3a,
    I32Store16 = 0x3b,
    I64Store8 = 0x3c,
    I64Store16 = 0x3d,
    I64Store32 = 0x3e,
}

impl MemOp {
    /// The alignment exponent of this access's width: 0 for one byte, 1 for
    /// two, 2 for four, 3 for eight. tinyvm rejects any memarg whose alignment
    /// *exceeds* this (`memarg`, "memory alignment exceeds natural
    /// alignment"), so it is both the default and the ceiling.
    pub(crate) fn natural_align(self) -> u32 {
        match self {
            MemOp::I32Load8S
            | MemOp::I32Load8U
            | MemOp::I64Load8S
            | MemOp::I64Load8U
            | MemOp::I32Store8
            | MemOp::I64Store8 => 0,
            MemOp::I32Load16S
            | MemOp::I32Load16U
            | MemOp::I64Load16S
            | MemOp::I64Load16U
            | MemOp::I32Store16
            | MemOp::I64Store16 => 1,
            MemOp::I32Load
            | MemOp::F32Load
            | MemOp::I64Load32S
            | MemOp::I64Load32U
            | MemOp::I32Store
            | MemOp::F32Store
            | MemOp::I64Store32 => 2,
            MemOp::I64Load | MemOp::F64Load | MemOp::I64Store | MemOp::F64Store => 3,
        }
    }
}

/// A load or store at its natural alignment -- what every producer emits and
/// what a `.wat` access with no explicit `align=` assembles to.
pub(crate) fn mem(out: &mut Vec<u8>, mem_op: MemOp, offset: u32) {
    mem_aligned(out, mem_op, mem_op.natural_align(), offset);
}

/// A load or store whose alignment hint is deliberately *below* natural. Above
/// natural is not an option the caller has: it is a producer bug, not user
/// input, and the load gate rejects it, so it is clamped and caught in debug.
pub(crate) fn mem_aligned(out: &mut Vec<u8>, mem_op: MemOp, align: u32, offset: u32) {
    debug_assert!(
        align <= mem_op.natural_align(),
        "memarg alignment {align} exceeds the natural alignment of {mem_op:?}"
    );
    out.push(mem_op as u8);
    unsigned(out, align.min(mem_op.natural_align()));
    unsigned(out, offset);
}

/// `memory.size` and `memory.grow` carry a memory index, which is `0` in every
/// module with one memory. It is a plain byte, not a LEB128 vector.
pub(crate) fn memory_size(out: &mut Vec<u8>, memory: u8) {
    out.push(0x3f);
    out.push(memory);
}

pub(crate) fn memory_grow(out: &mut Vec<u8>, memory: u8) {
    out.push(0x40);
    out.push(memory);
}

pub(crate) fn i32_const(out: &mut Vec<u8>, value: i32) {
    out.push(0x41);
    signed_32(out, value);
}

pub(crate) fn i64_const(out: &mut Vec<u8>, value: i64) {
    out.push(0x42);
    signed_64(out, value);
}

/// Floats are *not* LEB128: they are raw little-endian IEEE-754 bits. Encoding
/// them through the integer path is a real failure mode, which is why they go
/// through `to_bits` here and nowhere else.
pub(crate) fn f32_const(out: &mut Vec<u8>, value: f32) {
    out.push(0x43);
    out.extend_from_slice(&value.to_bits().to_le_bytes());
}

pub(crate) fn f64_const(out: &mut Vec<u8>, value: f64) {
    out.push(0x44);
    out.extend_from_slice(&value.to_bits().to_le_bytes());
}

pub(crate) fn local_get(out: &mut Vec<u8>, index: u32) {
    out.push(0x20);
    unsigned(out, index);
}

pub(crate) fn local_set(out: &mut Vec<u8>, index: u32) {
    out.push(0x21);
    unsigned(out, index);
}

pub(crate) fn local_tee(out: &mut Vec<u8>, index: u32) {
    out.push(0x22);
    unsigned(out, index);
}

pub(crate) fn global_get(out: &mut Vec<u8>, index: u32) {
    out.push(0x23);
    unsigned(out, index);
}

pub(crate) fn global_set(out: &mut Vec<u8>, index: u32) {
    out.push(0x24);
    unsigned(out, index);
}

pub(crate) fn call(out: &mut Vec<u8>, index: u32) {
    out.push(0x10);
    unsigned(out, index);
}

/// `call_indirect` names a *type* and a *table*, in that order. The type index
/// is what tinyvm matches the callee's signature against exactly (spec 4.4.8),
/// so this instruction is only as expressive as the type section is.
pub(crate) fn call_indirect(out: &mut Vec<u8>, type_index: u32, table_index: u32) {
    out.push(0x11);
    unsigned(out, type_index);
    unsigned(out, table_index);
}

// -- control flow -------------------------------------------------------------

/// The result shape of a block, loop or `if`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockType {
    Empty,
    Value(ValueType),
    /// A full function type, by index: the only way to give a block operands.
    TypeIndex(u32),
}

/// The block type is an `s33`, not a byte and not a `u32`.
///
/// That is the subtle one. The inline forms are small *negative* numbers
/// (`Empty` is -64, `i32` is -1), and a type index is the same field read as a
/// non-negative signed LEB128. So type index 64 encodes as `c0 00`, not as the
/// single byte `40` -- which is the reserved `Empty` encoding and would silently
/// mean something else. tinyvm's `block_type` says exactly this
/// (`crates/tinyvm/src/wasm.rs`).
pub(crate) fn block_type(out: &mut Vec<u8>, ty: BlockType) {
    match ty {
        BlockType::Empty => out.push(0x40),
        BlockType::Value(v) => out.push(v.byte()),
        BlockType::TypeIndex(index) => signed_64(out, i64::from(index)),
    }
}

pub(crate) fn block(out: &mut Vec<u8>, ty: BlockType) {
    out.push(0x02);
    block_type(out, ty);
}

pub(crate) fn loop_(out: &mut Vec<u8>, ty: BlockType) {
    out.push(0x03);
    block_type(out, ty);
}

pub(crate) fn if_(out: &mut Vec<u8>, ty: BlockType) {
    out.push(0x04);
    block_type(out, ty);
}

pub(crate) fn else_(out: &mut Vec<u8>) {
    out.push(0x05);
}

/// The `end` that closes a block, loop or `if`. The one that terminates a
/// *function* expression is not this: [`code_section`] appends that itself, so
/// no lowering pass can forget it.
pub(crate) fn end(out: &mut Vec<u8>) {
    out.push(END);
}

pub(crate) fn br(out: &mut Vec<u8>, depth: u32) {
    out.push(0x0c);
    unsigned(out, depth);
}

pub(crate) fn br_if(out: &mut Vec<u8>, depth: u32) {
    out.push(0x0d);
    unsigned(out, depth);
}

/// `br_table`: a vector of labels, then the default label outside the vector.
/// The default is not part of the count, and forgetting that shifts every
/// subsequent byte.
pub(crate) fn br_table(out: &mut Vec<u8>, targets: &[u32], default: u32) {
    out.push(0x0e);
    vector(out, targets, |out, t| unsigned(out, *t));
    unsigned(out, default);
}

// -- LEB128 --------------------------------------------------------------------

/// Unsigned LEB128, minimal length. "Minimal" matters: a validator may reject
/// a padded encoding, and a padded one is never what a canonical producer
/// emits.
pub(crate) fn unsigned(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Signed LEB128 for an `i32`, minimal length. Sign-extending to `i64` first
/// changes nothing: the minimal encoding of a value does not depend on the
/// width it was held in.
pub(crate) fn signed_32(out: &mut Vec<u8>, value: i32) {
    signed_64(out, i64::from(value));
}

/// Signed LEB128, minimal length.
///
/// The loop stops when the remaining bits are all copies of the sign bit *and*
/// the byte just written carries that sign in bit 6. Dropping either half of
/// that condition is the classic way to encode `-64` as `0x40` (which reads
/// back as `64`) or to emit a redundant trailing byte.
///
/// At the `i64` extreme this is also a *range* property, not only a size one:
/// the encoding is ten bytes and tinyvm rejects a tenth byte that is anything
/// but `0x00` or `0x7f` (`leb_s64`). Minimality is what keeps it inside that.
pub(crate) fn signed_64(out: &mut Vec<u8>, value: i64) {
    let mut remaining = value;
    loop {
        let byte = (remaining & 0x7f) as u8;
        // Arithmetic shift: the sign extends, so a negative value converges on
        // -1 rather than on 0.
        remaining >>= 7;
        let sign_bit_set = byte & 0x40 != 0;
        if (remaining == 0 && !sign_bit_set) || (remaining == -1 && sign_bit_set) {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
