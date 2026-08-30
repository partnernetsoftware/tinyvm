//! A wasm-shaped module, still in Rust types.
//!
//! This sits between the tree and the bytes on purpose. Lowering decides *what
//! wasm* to build; [`super::encode`] decides *what bytes* that wasm is. Keeping
//! them apart is what lets the encoder be strict on its own terms -- canonical
//! section order, minimal LEB128, exact expression termination -- without the
//! lowering having to know any of it, and what lets M1's control flow be tested
//! as instruction sequences before a single byte is written.
//!
//! The vocabulary is only as wide as M0 needs. Every milestone adds variants;
//! none of them changes this file's role.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValType {
    I32,
}

/// A function signature. Both vectors are the wasm ones, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FuncType {
    pub(crate) params: Vec<ValType>,
    pub(crate) results: Vec<ValType>,
}

/// A function imported from the host. wasm gives imported functions the first
/// function indices, before every defined one, which is why [`Module`] holds
/// them in order rather than in a map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Import {
    pub(crate) module: String,
    pub(crate) name: String,
    pub(crate) type_index: u32,
}

/// One instruction. Named after the wasm opcode it becomes, not after the
/// JavaScript operator it came from -- the mapping between those two stops
/// being one-to-one as soon as JS numbers are real numbers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Ins {
    I32Const(i32),
    LocalGet(u32),
    /// Call the function at this index. Imports come first, so an index below
    /// the import count is a host call.
    Call(u32),
    I32Add,
    I32Sub,
    I32Mul,
    I32DivS,
    I32RemS,
}

/// A defined function: its type, its declared locals beyond the parameters as
/// run-length `(count, type)` groups, and its body.
///
/// The body does *not* carry the terminating `end`. That byte is part of how an
/// expression is encoded, not a choice the lowering makes, so it belongs to the
/// encoder -- and keeping it there means no lowering pass can forget it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Func {
    pub(crate) type_index: u32,
    pub(crate) locals: Vec<(u32, ValType)>,
    pub(crate) body: Vec<Ins>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportKind {
    Func,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Export {
    pub(crate) name: String,
    pub(crate) kind: ExportKind,
    pub(crate) index: u32,
}

/// A whole module. No memory, tables, globals, or start function yet, so those
/// sections have no fields here rather than empty ones: an empty vector would
/// be a section the encoder must decide whether to emit, and a decision no
/// caller can influence is not worth having. `imports` is the exception --
/// whether there are any is exactly the caller's decision.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Module {
    pub(crate) types: Vec<FuncType>,
    pub(crate) imports: Vec<Import>,
    pub(crate) funcs: Vec<Func>,
    pub(crate) exports: Vec<Export>,
}

/// The M1 vocabulary: a module with a memory, globals, data and many
/// functions, over the instruction set [`super::repr`]'s V1 values need.
///
/// Nested rather than replacing the items above because [`super::encode`]
/// matches exhaustively on the M0 [`Ins`] and lives in another lane, and a
/// widened `Ins` there is a crate that does not build. Integration is one
/// move: delete the M0 items, un-nest this module, and take [`m1::assemble`]
/// with it (see that function).
pub(crate) mod m1 {
    /// The value types V1 needs. `super::ValType` is M0's, which is `i32`
    /// alone.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ValType {
        I32,
        I64,
        F64,
    }

    /// wasm 1.0's only block type. A block that yields a value would need
    /// either an inline value type or a type index, and no lowering site wants
    /// one: a JS value is two words, so a block that produced one would need a
    /// multi-value type index, and a scratch local is cheaper to read.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum BlockType {
        Empty,
    }

    /// One instruction, named after the wasm opcode it becomes.
    ///
    /// `super::repr` currently declares a duplicate of this enum, because
    /// `repr.rs` landed before `ir.rs` could name an `i64`; its header says so
    /// and says the definition belongs here. This *is* that definition, and
    /// `super::super::emit::m1` holds the one bridge between the two until
    /// `repr.rs` and `runtime.rs` can name it directly. `ir.rs` deliberately
    /// does not reach for `repr` itself: the encoder's tests compile this file
    /// on its own, so it has to stand on its own.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub(crate) enum Ins {
        // control
        Block(BlockType),
        Loop(BlockType),
        If(BlockType),
        End,
        Br(u32),
        BrIf(u32),
        Return,
        Call(u32),
        /// `call_indirect(type, table)`. The type index is what the callee's
        /// signature is matched against, exactly (spec 4.4.8), so this
        /// instruction is only as expressive as the type section is -- which
        /// is why [`super::super::emit`] interns one uniform signature for
        /// every call through a value and adapts the arity around it.
        CallIndirect(u32, u32),
        Unreachable,
        Drop,
        // variables
        LocalGet(u32),
        LocalSet(u32),
        LocalTee(u32),
        GlobalGet(u32),
        GlobalSet(u32),
        // memory, as (align exponent, offset)
        I32Load(u32, u32),
        I32Load8U(u32, u32),
        I32Store(u32, u32),
        I32Store8(u32, u32),
        /// The whole `i64` of a V1 pair's payload, in and out of an object
        /// record's value slot. The alignment is an *exponent* the caller
        /// chooses, and every emitter of these passes 2: the bump allocator
        /// aligns to four bytes, so eight-byte alignment is not something the
        /// module may claim. Below-natural is legal wasm and is a hint only.
        I64Load(u32, u32),
        I64Store(u32, u32),
        MemorySize,
        MemoryGrow,
        // constants
        I32Const(i32),
        I64Const(i64),
        F64Const(f64),
        // i32
        I32Eqz,
        I32Eq,
        I32Ne,
        I32LtS,
        I32LtU,
        I32GeU,
        I32Add,
        I32Sub,
        I32Mul,
        I32DivS,
        I32RemS,
        I32And,
        I32Or,
        I32Shl,
        I32ShrU,
        I32Xor,
        I32ShrS,
        // i64
        I64Eq,
        I64Add,
        // f64
        F64Eq,
        F64Ne,
        F64Lt,
        F64Gt,
        F64Le,
        F64Ge,
        F64Abs,
        F64Neg,
        F64Ceil,
        F64Floor,
        F64Nearest,
        F64Sqrt,
        F64Min,
        F64Max,
        F64Add,
        F64Sub,
        F64Mul,
        F64Div,
        F64Copysign,
        F64Trunc,
        // conversions
        I32TruncF64S,
        I32WrapI64,
        I64ExtendI32U,
        F64ConvertI32S,
        F64ConvertI32U,
        F64ReinterpretI64,
        I64ReinterpretF64,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct FuncType {
        pub(crate) params: Vec<ValType>,
        pub(crate) results: Vec<ValType>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Import {
        pub(crate) module: String,
        pub(crate) name: String,
        pub(crate) type_index: u32,
    }

    /// A defined function. The body does not carry its terminating `end`, for
    /// the reason the M0 [`super::Func`] gives.
    ///
    /// `name` is for the `name` custom section and nothing else: it never
    /// reaches the import or export tables, so naming a function here cannot
    /// widen what a host can reach. A trap inside `__add` is worth being able
    /// to read as `__add`.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Func {
        pub(crate) name: Option<String>,
        /// Where the function was written, for the `qjs.lines` custom
        /// section; `None` for a function that has no place an author could
        /// open (the runtime's, the script's own).
        pub(crate) site: Option<Site>,
        pub(crate) type_index: u32,
        pub(crate) locals: Vec<(u32, ValType)>,
        pub(crate) body: Vec<Ins>,
    }

    /// A function export. Only functions are exported: a guest's memory and
    /// globals are the engine's, and handing them out by name would let a host
    /// reach past the representation.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Export {
        pub(crate) name: String,
        pub(crate) index: u32,
    }

    /// A defined global's initial value. One instruction, which is all a
    /// producer needs -- see [`super::super::encode::ConstExpr`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Const {
        I32(i32),
        I64(i64),
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Global {
        pub(crate) ty: ValType,
        pub(crate) mutable: bool,
        pub(crate) init: Const,
    }

    /// A defined table. `funcref` is the only element type this compiler
    /// emits: the table exists so a function value can be called, and a
    /// function value is the only reference the representation has.
    ///
    /// No declared maximum, for the reason [`Memory`]'s minimum gives from the
    /// other side: the table's size is fixed at compile time by how many
    /// functions became values, so a maximum would restate the minimum.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Table {
        pub(crate) min: u32,
        pub(crate) max: Option<u32>,
    }

    /// An active element segment in table 0: function indices written at
    /// `offset` when the module instantiates.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Elem {
        pub(crate) offset: u32,
        pub(crate) funcs: Vec<u32>,
    }

    /// An active data segment in memory 0.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Data {
        pub(crate) offset: u32,
        pub(crate) bytes: Vec<u8>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Memory {
        pub(crate) min: u32,
        pub(crate) max: Option<u32>,
    }

    /// A whole module. `memory` is an `Option` because it is the one section
    /// here whose presence a caller can still choose; everything else is a
    /// vector that is emitted when it has entries.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Module {
        pub(crate) types: Vec<FuncType>,
        pub(crate) imports: Vec<Import>,
        pub(crate) table: Option<Table>,
        pub(crate) memory: Option<Memory>,
        pub(crate) globals: Vec<Global>,
        pub(crate) funcs: Vec<Func>,
        pub(crate) elements: Vec<Elem>,
        pub(crate) data: Vec<Data>,
        pub(crate) exports: Vec<Export>,
    }

    // -- assembly --------------------------------------------------------
    //
    // Everything below decides *which encoder writer* an IR item goes
    // through; the writers decide the bytes, and no byte, LEB128 or section
    // id is spelled here. It is the M1 twin of `encode::encode` and belongs
    // beside it -- it is here only because `encode.rs` is another lane's
    // file, and it moves there whole.

    use crate::encode::{
        self, ConstExpr, Data as EncData, Element, FuncBody, ImportDesc, ImportEntry, MemOp, Op,
        Signature, ValueType,
    };

    pub(crate) fn assemble(module: &Module) -> Vec<u8> {
        let types: Vec<Signature> = module.types.iter().map(signature).collect();
        let imports: Vec<ImportEntry> = module
            .imports
            .iter()
            .map(|i| ImportEntry {
                module: i.module.clone(),
                name: i.name.clone(),
                desc: ImportDesc::Func(i.type_index),
            })
            .collect();
        let type_indices: Vec<u32> = module.funcs.iter().map(|f| f.type_index).collect();
        let globals: Vec<encode::Global> = module
            .globals
            .iter()
            .map(|g| encode::Global {
                ty: value_type(g.ty),
                mutable: g.mutable,
                init: match g.init {
                    Const::I32(v) => ConstExpr::I32(v),
                    Const::I64(v) => ConstExpr::I64(v),
                },
            })
            .collect();
        let exports: Vec<encode::ExportEntry> = module
            .exports
            .iter()
            .map(|e| encode::ExportEntry {
                name: e.name.clone(),
                descriptor: encode::DESCRIPTOR_FUNC,
                index: e.index,
            })
            .collect();
        let bodies: Vec<FuncBody> = module
            .funcs
            .iter()
            .map(|f| FuncBody {
                locals: f
                    .locals
                    .iter()
                    .map(|(count, ty)| (*count, value_type(*ty)))
                    .collect(),
                code: {
                    let mut code = Vec::new();
                    for ins in &f.body {
                        write(&mut code, *ins);
                    }
                    code
                },
            })
            .collect();
        let elements: Vec<Element> = module
            .elements
            .iter()
            .map(|e| Element::ActiveFuncs {
                table: 0,
                offset: ConstExpr::I32(e.offset as i32),
                funcs: e.funcs.clone(),
            })
            .collect();
        let data: Vec<EncData> = module
            .data
            .iter()
            .map(|d| EncData::Active {
                memory: 0,
                offset: ConstExpr::I32(d.offset as i32),
                bytes: d.bytes.clone(),
            })
            .collect();

        // Rank order, which is not id order: see `encode`'s section table.
        let mut out = encode::HEADER.to_vec();
        encode::type_section(&mut out, &types);
        if !imports.is_empty() {
            encode::import_section(&mut out, &imports);
        }
        encode::function_section(&mut out, &type_indices);
        if let Some(table) = module.table {
            encode::table_section(
                &mut out,
                &[encode::TableType {
                    element: ValueType::FuncRef,
                    limits: encode::Limits {
                        min: table.min,
                        max: table.max,
                    },
                }],
            );
        }
        if let Some(memory) = module.memory {
            encode::memory_section(
                &mut out,
                &[encode::Limits {
                    min: memory.min,
                    max: memory.max,
                }],
            );
        }
        if !globals.is_empty() {
            encode::global_section(&mut out, &globals);
        }
        encode::export_section(&mut out, &exports);
        if !elements.is_empty() {
            encode::element_section(&mut out, &elements);
        }
        encode::code_section(&mut out, &bodies);
        // No data-count section: it is required only by `memory.init` and
        // `data.drop`, and this compiler emits neither.
        if !data.is_empty() {
            encode::data_section(&mut out, &data);
        }
        name_section(&mut out, module);
        lines_section(&mut out, module);
        out
    }

    /// Where a function was written: the 1-based line, and the 1-based
    /// column on it in UTF-16 code units, the way an editor counts.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub(crate) struct Site {
        pub(crate) line: u32,
        pub(crate) column: u32,
    }

    /// The `qjs.lines` custom section: where each function was written, as
    /// a vector of `(function index, 1-based line, 1-based column)` triples
    /// in index order, beside the `name` section and read the same way --
    /// tinyvm's `from_bytes_explained` looks it up only after a body has
    /// failed validation, so a refusal can say `in function \`f\` (#12)
    /// (line 7, column 12)` instead of sending the author to bisect. Absent
    /// when no function has a site, which keeps a program without functions
    /// byte-identical to what it was before the section existed.
    fn lines_section(out: &mut Vec<u8>, module: &Module) {
        let sited: Vec<(u32, Site)> = module
            .funcs
            .iter()
            .enumerate()
            .filter_map(|(position, f)| {
                let site = f.site?;
                Some(((module.imports.len() + position) as u32, site))
            })
            .collect();
        if sited.is_empty() {
            return;
        }
        let mut contents = Vec::new();
        encode::vector(&mut contents, &sited, |body, (index, site)| {
            encode::unsigned(body, *index);
            encode::unsigned(body, site.line);
            encode::unsigned(body, site.column);
        });
        encode::custom_section(out, LINES_SECTION, &contents);
    }

    /// The custom section's name. Shared with tinyvm by spelling, not by
    /// code: the reader there walks raw bytes and knows only this string.
    pub(crate) const LINES_SECTION: &str = "qjs.lines";

    /// The `name` custom section, function names only.
    ///
    /// A custom section carries no semantics -- tinyvm reads its name and
    /// skips its contents -- so this cannot change what the module does. The
    /// indices are function indices, which means the imports come first and a
    /// defined function is offset by the import count.
    fn name_section(out: &mut Vec<u8>, module: &Module) {
        let named: Vec<(u32, &str)> = module
            .funcs
            .iter()
            .enumerate()
            .filter_map(|(position, f)| {
                let name = f.name.as_deref()?;
                Some(((module.imports.len() + position) as u32, name))
            })
            .collect();
        if named.is_empty() {
            return;
        }
        // Subsection 1 is the function-name map: its own id and byte length,
        // then a vector of (index, name) in index order.
        let mut map = Vec::new();
        encode::vector(&mut map, &named, |body, (index, text)| {
            encode::unsigned(body, *index);
            encode::name(body, text);
        });
        let mut contents = vec![NAME_SUBSECTION_FUNCTIONS];
        encode::unsigned(&mut contents, map.len() as u32);
        contents.extend_from_slice(&map);
        encode::custom_section(out, "name", &contents);
    }

    /// The `name` section's function-name subsection id.
    const NAME_SUBSECTION_FUNCTIONS: u8 = 1;

    fn signature(ty: &FuncType) -> Signature {
        Signature {
            params: ty.params.iter().copied().map(value_type).collect(),
            results: ty.results.iter().copied().map(value_type).collect(),
        }
    }

    fn value_type(ty: ValType) -> ValueType {
        match ty {
            ValType::I32 => ValueType::I32,
            ValType::I64 => ValueType::I64,
            ValType::F64 => ValueType::F64,
        }
    }

    fn block(ty: BlockType) -> encode::BlockType {
        match ty {
            BlockType::Empty => encode::BlockType::Empty,
        }
    }

    fn write(out: &mut Vec<u8>, ins: Ins) {
        match ins {
            Ins::Block(ty) => encode::block(out, block(ty)),
            Ins::Loop(ty) => encode::loop_(out, block(ty)),
            Ins::If(ty) => encode::if_(out, block(ty)),
            Ins::End => encode::end(out),
            Ins::Br(depth) => encode::br(out, depth),
            Ins::BrIf(depth) => encode::br_if(out, depth),
            Ins::Return => encode::op(out, Op::Return),
            Ins::Call(index) => encode::call(out, index),
            Ins::CallIndirect(ty, table) => encode::call_indirect(out, ty, table),
            Ins::Unreachable => encode::op(out, Op::Unreachable),
            Ins::Drop => encode::op(out, Op::Drop),
            Ins::LocalGet(i) => encode::local_get(out, i),
            Ins::LocalSet(i) => encode::local_set(out, i),
            Ins::LocalTee(i) => encode::local_tee(out, i),
            Ins::GlobalGet(i) => encode::global_get(out, i),
            Ins::GlobalSet(i) => encode::global_set(out, i),
            Ins::I32Load(align, offset) => encode::mem_aligned(out, MemOp::I32Load, align, offset),
            Ins::I32Load8U(align, offset) => {
                encode::mem_aligned(out, MemOp::I32Load8U, align, offset)
            }
            Ins::I32Store(align, offset) => {
                encode::mem_aligned(out, MemOp::I32Store, align, offset)
            }
            Ins::I32Store8(align, offset) => {
                encode::mem_aligned(out, MemOp::I32Store8, align, offset)
            }
            Ins::I64Load(align, offset) => encode::mem_aligned(out, MemOp::I64Load, align, offset),
            Ins::I64Store(align, offset) => {
                encode::mem_aligned(out, MemOp::I64Store, align, offset)
            }
            Ins::MemorySize => encode::memory_size(out, 0),
            Ins::MemoryGrow => encode::memory_grow(out, 0),
            Ins::I32Const(v) => encode::i32_const(out, v),
            Ins::I64Const(v) => encode::i64_const(out, v),
            Ins::F64Const(v) => encode::f64_const(out, v),
            Ins::I32Eqz => encode::op(out, Op::I32Eqz),
            Ins::I32Eq => encode::op(out, Op::I32Eq),
            Ins::I32Ne => encode::op(out, Op::I32Ne),
            Ins::I32LtS => encode::op(out, Op::I32LtS),
            Ins::I32LtU => encode::op(out, Op::I32LtU),
            Ins::I32GeU => encode::op(out, Op::I32GeU),
            Ins::I32Add => encode::op(out, Op::I32Add),
            Ins::I32Sub => encode::op(out, Op::I32Sub),
            Ins::I32Mul => encode::op(out, Op::I32Mul),
            Ins::I32DivS => encode::op(out, Op::I32DivS),
            Ins::I32RemS => encode::op(out, Op::I32RemS),
            Ins::I32And => encode::op(out, Op::I32And),
            Ins::I32Or => encode::op(out, Op::I32Or),
            Ins::I32Shl => encode::op(out, Op::I32Shl),
            Ins::I32ShrU => encode::op(out, Op::I32ShrU),
            Ins::I32Xor => encode::op(out, Op::I32Xor),
            Ins::I32ShrS => encode::op(out, Op::I32ShrS),
            Ins::I64Eq => encode::op(out, Op::I64Eq),
            Ins::I64Add => encode::op(out, Op::I64Add),
            Ins::F64Eq => encode::op(out, Op::F64Eq),
            Ins::F64Ne => encode::op(out, Op::F64Ne),
            Ins::F64Lt => encode::op(out, Op::F64Lt),
            Ins::F64Gt => encode::op(out, Op::F64Gt),
            Ins::F64Le => encode::op(out, Op::F64Le),
            Ins::F64Ge => encode::op(out, Op::F64Ge),
            Ins::F64Abs => encode::op(out, Op::F64Abs),
            Ins::F64Neg => encode::op(out, Op::F64Neg),
            Ins::F64Ceil => encode::op(out, Op::F64Ceil),
            Ins::F64Floor => encode::op(out, Op::F64Floor),
            Ins::F64Nearest => encode::op(out, Op::F64Nearest),
            Ins::F64Sqrt => encode::op(out, Op::F64Sqrt),
            Ins::F64Min => encode::op(out, Op::F64Min),
            Ins::F64Max => encode::op(out, Op::F64Max),
            Ins::F64Add => encode::op(out, Op::F64Add),
            Ins::F64Sub => encode::op(out, Op::F64Sub),
            Ins::F64Mul => encode::op(out, Op::F64Mul),
            Ins::F64Div => encode::op(out, Op::F64Div),
            Ins::F64Copysign => encode::op(out, Op::F64Copysign),
            Ins::F64Trunc => encode::op(out, Op::F64Trunc),
            Ins::I32TruncF64S => encode::op(out, Op::I32TruncF64S),
            Ins::I32WrapI64 => encode::op(out, Op::I32WrapI64),
            Ins::I64ExtendI32U => encode::op(out, Op::I64ExtendI32U),
            Ins::F64ConvertI32S => encode::op(out, Op::F64ConvertI32S),
            Ins::F64ConvertI32U => encode::op(out, Op::F64ConvertI32U),
            Ins::F64ReinterpretI64 => encode::op(out, Op::F64ReinterpretI64),
            Ins::I64ReinterpretF64 => encode::op(out, Op::I64ReinterpretF64),
        }
    }
}
