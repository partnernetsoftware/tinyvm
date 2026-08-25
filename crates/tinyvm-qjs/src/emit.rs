//! [`super::ast::Program`] -> [`super::ir::Module`].
//!
//! The whole program becomes one exported function named `main`, taking as many
//! `i32` parameters as the highest `$N` the source names, and returning `i32`.
//! Every host name the source used becomes a zero-argument `js.<name>` import,
//! in sorted order, so the same source always produces the same import table.
//! Nothing here can fail: the parser has already rejected everything this
//! subset cannot lower, and it rejected it with a diagnostic that named the
//! boundary. Once lowering *can* fail (an unresolved name, when the language
//! grows real bindings), this returns a `Result` and the AST grows the spans
//! that diagnostic needs.
//!
//! # A JavaScript divergence we chose, on purpose
//!
//! `/` and `%` lower to `i32.div_s` and `i32.rem_s`, which **trap** when the
//! divisor is zero, and `i32.div_s` also traps on `i32::MIN / -1`. JavaScript
//! has one number type, so `1/0` is `Infinity`, `1%0` is `NaN`, and
//! `-2147483648 / -1` is `2147483648`. M0 has only `i32`: none of those three
//! values exists here, so there is no way to be right, only ways to be wrong.
//!
//! The alternative was to guard every division and yield `0`. That was
//! rejected. A fabricated `0` is a wrong number that flows on into the rest of
//! the expression and is indistinguishable from a real result, which is exactly
//! the silent corruption this whole stack refuses; a trap is loud, arrives as a
//! typed `QjswasmError::Trap`, and costs nothing per division. It is also the
//! divergence that is cheapest to retire: when floats land (M3) these three
//! cases start producing `Infinity`/`NaN`/`2147483648`, and no script can have
//! come to depend on `0` in the meantime, because every such script failed
//! loudly instead of quietly computing the wrong answer.
//!
//! Locked by `div_and_rem_by_zero_trap` and `signed_division_overflow_traps`
//! in `tests/qjs_subset.rs`.

use std::collections::BTreeSet;

use super::ast::{BinaryOp, Expr, Program, UnaryOp};
use super::ir::{Export, ExportKind, Func, FuncType, Import, Ins, Module, ValType};

/// The name every compiled `.qjs` exports. One entry point, fixed, so the host
/// never has to guess what to call.
pub(crate) const ENTRY: &str = "main";

/// The module every host name is imported from. One module name, so the world
/// a guest can see is a single flat table and not a namespace to explore.
pub(crate) const HOST_MODULE: &str = "js";

pub(crate) fn lower(program: &Program) -> Module {
    let mut hosts = BTreeSet::new();
    collect_hosts(&program.expr, &mut hosts);
    let hosts: Vec<String> = hosts.into_iter().collect();

    let mut types: Vec<FuncType> = Vec::new();
    // Import types first: imported functions come first in the index space, so
    // emitting their type first is what keeps a name/ops source byte-identical
    // to what the previous single-pass encoder produced.
    let host_type = if hosts.is_empty() {
        0
    } else {
        intern(
            &mut types,
            FuncType {
                params: Vec::new(),
                results: vec![ValType::I32],
            },
        )
    };
    let arity = argument_count(&program.expr);
    let main_type = intern(
        &mut types,
        FuncType {
            params: vec![ValType::I32; arity as usize],
            results: vec![ValType::I32],
        },
    );

    let mut body = Vec::new();
    emit(&program.expr, &hosts, &mut body);

    Module {
        types,
        imports: hosts
            .iter()
            .map(|name| Import {
                module: HOST_MODULE.to_string(),
                name: name.clone(),
                type_index: host_type,
            })
            .collect(),
        funcs: vec![Func {
            type_index: main_type,
            locals: Vec::new(),
            body,
        }],
        exports: vec![Export {
            name: ENTRY.to_string(),
            kind: ExportKind::Func,
            index: hosts.len() as u32,
        }],
    }
}

/// Index of `wanted` in `types`, appending it first if it is not there. Two
/// functions with the same signature must share one type index; a duplicate
/// entry is legal wasm but is not what a canonical producer emits.
fn intern(types: &mut Vec<FuncType>, wanted: FuncType) -> u32 {
    if let Some(index) = types.iter().position(|t| *t == wanted) {
        return index as u32;
    }
    types.push(wanted);
    (types.len() - 1) as u32
}

/// Every host name the expression mentions. A `BTreeSet` so the import table
/// is sorted and deduplicated: `g+g` imports `js.g` once, and the import order
/// depends on the names rather than on where they appear.
fn collect_hosts(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Int(_) | Expr::Arg(_) => {}
        Expr::Host(name) => {
            out.insert(name.clone());
        }
        Expr::Unary(_, operand) => collect_hosts(operand, out),
        Expr::Binary(_, lhs, rhs) => {
            collect_hosts(lhs, out);
            collect_hosts(rhs, out);
        }
    }
}

/// How many parameters `main` declares: one past the highest `$N` used, so
/// `$2` alone still means three, and a source naming no argument means zero.
fn argument_count(expr: &Expr) -> u32 {
    match expr {
        Expr::Int(_) | Expr::Host(_) => 0,
        Expr::Arg(index) => index + 1,
        Expr::Unary(_, operand) => argument_count(operand),
        Expr::Binary(_, lhs, rhs) => argument_count(lhs).max(argument_count(rhs)),
    }
}

/// Post-order emission: operands first, then the operator. That is the wasm
/// stack discipline, and it is why the tree needs no register allocation.
fn emit(expr: &Expr, hosts: &[String], out: &mut Vec<Ins>) {
    match expr {
        Expr::Int(value) => out.push(Ins::I32Const(*value)),
        // Parameters occupy the first local indices, in order, so `$N` is
        // local `N` with nothing to look up.
        Expr::Arg(index) => out.push(Ins::LocalGet(*index)),
        // Imports occupy the first function indices, in the order of the
        // import section, which is the order of `hosts`.
        Expr::Host(name) => {
            let index = hosts
                .iter()
                .position(|h| h == name)
                .expect("every host name in the tree was collected");
            out.push(Ins::Call(index as u32));
        }
        // wasm has no integer negation instruction. `0 - x` is the standard
        // encoding and is what a wasm producer would emit.
        Expr::Unary(UnaryOp::Neg, operand) => {
            out.push(Ins::I32Const(0));
            emit(operand, hosts, out);
            out.push(Ins::I32Sub);
        }
        Expr::Binary(op, lhs, rhs) => {
            emit(lhs, hosts, out);
            emit(rhs, hosts, out);
            out.push(match op {
                BinaryOp::Add => Ins::I32Add,
                BinaryOp::Sub => Ins::I32Sub,
                BinaryOp::Mul => Ins::I32Mul,
                BinaryOp::Div => Ins::I32DivS,
                BinaryOp::Rem => Ins::I32RemS,
            });
        }
    }
}

/// The M1 lowering: [`super::ast::m1::Program`] -> [`super::ir::m1::Module`].
///
/// Nested beside the M0 lowering above rather than replacing it, for the
/// reason `ast::m1` gives: the M0 pipeline has callers that are green on `i32`
/// in and `i32` out, and this one moves V1 pairs. Integration is one move --
/// delete the M0 items, un-nest this module.
///
/// # The shape of the module this produces
///
/// ```text
/// function index 0..I     the host imports, `js.<name>`, sorted by name
///                 I..I+R  the emitted runtime, in `runtime::SET` order
///                 I+R..   the script, then every nested function by FuncId
/// global    index 0       the bump-allocation pointer
///                 1..     two globals per script binding
/// ```
///
/// The script is exported as [`ENTRY`]. It is an ordinary function of the
/// program's `$N`, which is what makes `return` in it mean what it says.
///
/// # Storage: why the script's bindings are globals
///
/// A wasm local belongs to a frame. A nested function may read a script
/// binding (the front end resolves that to `Res::Global`, and refuses any
/// deeper capture), so script storage has to outlive the script's frame --
/// which a local does not. Every script binding is therefore two mutable
/// globals, and every other function's bindings are locals. The decision is
/// made from the *binding*, never from the `Res` variant: `Res::Local` and
/// `Res::Global` say where the reference is, not where the storage is.
///
/// # Where the `end`s come from
///
/// Nothing here emits `else`. [`super::repr`]'s instruction set has no such
/// variant, and the two-block form below needs none:
///
/// ```text
/// block                      ;; depth 1 from inside the inner block
///   block                    ;; depth 0
///     <test>; br_if 0        ;; the test is *inverted*: branch to the else
///     <then>
///     br 1
///   end
///   <else>
/// end
/// ```
///
/// Every branch this module emits targets a label it opened itself, at a
/// nesting it knows statically, so no depth is ever computed from a stack of
/// enclosing labels. That is deliberate: the M1 statement set has no `break`
/// and no `continue` -- the two constructs whose branch target is *not* local
/// -- so a label stack would be machinery with one possible answer. It is what
/// the milestone that adds them must build.
pub(crate) mod m1 {
    use std::collections::BTreeMap;

    use crate::ast::m1 as ast;
    use crate::diag::{Boundary, CompileError, host_table, unsupported};
    use crate::ir::m1 as ir;
    use crate::opts::{HostFn, HostParam, HostResult, Names, Options};
    use crate::repr::{
        self, BlockType, Ins, ValType, WIDTH, box_bool, box_number, box_object, box_string,
        const_bool, const_null, const_number, const_string, const_undefined, drop_value,
        load_local, store_local, unbox_number, unbox_string,
    };
    use crate::runtime::{self, Ctx, FnBuild, Rt, STRING_HEADER, StringPool};

    /// The name every compiled script exports, as at M0.
    pub(crate) const ENTRY: &str = super::ENTRY;
    pub(crate) const HOST_MODULE: &str = super::HOST_MODULE;

    /// Linear memory's page size, and wasm's own ceiling on how many pages a
    /// module may declare.
    const PAGE_BYTES: u32 = 65_536;
    const WASM_MAX_PAGES: u32 = 65_536;

    /// At least one page, and enough of them to hold the string literal pool.
    ///
    /// The declared *minimum* is what tinyvm checks an active data segment
    /// against at load time, so a module whose literals spill past what it
    /// declares is a module this compiler already decided was wasm and the load
    /// gate then refuses -- a successful compile whose output cannot be loaded,
    /// which is the one outcome the pipeline must never produce.
    ///
    /// No declared maximum. A ceiling written here would cap every guest at the
    /// same size whatever the embedder configured, and the bound belongs to the
    /// host's [`tinyvm::Limits`] -- which is what `runtime`'s allocator says it
    /// is relying on.
    fn memory_pages(pool: &StringPool) -> Result<u32, CompileError> {
        let bytes = pool.heap_start() as u32;
        let pages = bytes.div_ceil(PAGE_BYTES).max(1);
        if pages > WASM_MAX_PAGES {
            return Err(unsupported(
                Boundary::Subset,
                "a script whose string literals need more than 4 GiB of guest memory",
                0,
            ));
        }
        Ok(pages)
    }

    /// Global 0 is the bump-allocation pointer, which is what
    /// [`Ctx::heap_global`] names; the script's bindings take two globals each
    /// after it.
    const HEAP_GLOBAL: u32 = 0;
    const BINDING_GLOBALS: u32 = HEAP_GLOBAL + 1;

    /// A host name and the argument count every use of it agrees on.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Host {
        name: String,
        arity: u32,
    }

    /// One declaration the script uses, with the function indices its imports
    /// took. A `Bytes` result is two imports, so there are two indices.
    #[derive(Debug, Clone)]
    struct Bound {
        decl: HostFn,
        /// The length import of a [`HostResult::Bytes`] result.
        length: Option<u32>,
        /// The declaration's own import.
        index: u32,
    }

    /// What a host call in the tree resolves against.
    ///
    /// Two worlds, and the lowering of a call is genuinely different in each,
    /// which is why this is an enum and not a flag. Under
    /// [`Names::HostImport`] the import speaks this engine's V1 pairs and the
    /// call is a straight forward of the arguments. Under [`Names::Declared`]
    /// the import speaks raw wasm and the call has to unwrap every argument
    /// onto it and rewrap what comes back.
    #[derive(Debug, Clone)]
    enum Table {
        Pairs(Vec<Host>),
        Raw(Vec<Bound>),
    }

    impl Table {
        /// How many wasm imports the module has. Under `Raw` a declaration may
        /// be more than one.
        fn imports(&self) -> u32 {
            match self {
                Self::Pairs(hosts) => hosts.len() as u32,
                Self::Raw(bound) => bound
                    .iter()
                    .map(|b| 1 + u32::from(b.length.is_some()))
                    .sum(),
            }
        }
    }

    pub(crate) fn lower(
        program: &ast::Program,
        options: &Options,
    ) -> Result<ir::Module, CompileError> {
        let scan = scan(program)?;
        let table = match &options.names {
            Names::Declared(decls) => Table::Raw(bind(decls, &scan)?),
            _ => Table::Pairs(scan.hosts()),
        };
        // The pool opens before the runtime is built, because `__typeof`
        // answers with pool records and so has to know their addresses. The
        // five names go in first when the program asks for them, and not at
        // all when it does not.
        let mut pool = StringPool::default();
        let ctx = Ctx {
            // Imported functions take the first indices, so the runtime
            // starts exactly where the import table ends.
            func_base: table.imports(),
            heap_global: HEAP_GLOBAL,
            type_names: scan.type_of.then(|| runtime::TypeNames::intern(&mut pool)),
            key_names: scan
                .computed_key
                .then(|| runtime::KeyNames::intern(&mut pool)),
        };
        let user_base = ctx.func_base + runtime::SET.len() as u32;

        let mut types: Vec<ir::FuncType> = Vec::new();
        let imports: Vec<ir::Import> = match &table {
            Table::Pairs(hosts) => hosts
                .iter()
                .map(|host| ir::Import {
                    module: HOST_MODULE.to_string(),
                    name: host.name.clone(),
                    type_index: intern(&mut types, values(host.arity), values(1)),
                })
                .collect(),
            Table::Raw(bound) => raw_imports(bound, &mut types),
        };

        let mut funcs: Vec<ir::Func> = Vec::new();
        for built in runtime::build(&ctx) {
            let type_index = intern(&mut types, built.params.clone(), built.results.clone());
            funcs.push(func(
                built.name.to_string(),
                type_index,
                built.locals,
                built.body,
            ));
        }

        for (index, function) in program.functions.iter().enumerate() {
            let id = ast::FuncId(index as u32);
            let built = Lower::new(program, &ctx, &mut pool, &table, user_base, id).function()?;
            let arity = if id == ast::Program::SCRIPT {
                program.arg_count
            } else {
                function.params.len() as u32
            };
            let type_index = intern(&mut types, values(arity), values(1));
            funcs.push(func(
                debug_name(program, id),
                type_index,
                built.local_groups(),
                built.body,
            ));
        }

        // The bump pointer starts after the literals, so the pool has to be
        // full before this global can be built.
        let mut globals = vec![ir::Global {
            ty: ir::ValType::I32,
            mutable: true,
            init: ir::Const::I32(pool.heap_start()),
        }];
        for _ in 0..program.script().bindings.len() {
            // `(0, 0)` is `undefined`, which is what an unwritten binding
            // means -- see `repr`'s note on why `TAG_UNDEFINED` is zero.
            globals.push(ir::Global {
                ty: ir::ValType::I32,
                mutable: true,
                init: ir::Const::I32(0),
            });
            globals.push(ir::Global {
                ty: ir::ValType::I64,
                mutable: true,
                init: ir::Const::I64(0),
            });
        }

        let data = if pool.is_empty() {
            Vec::new()
        } else {
            let (offset, bytes) = pool.segment();
            vec![ir::Data {
                offset,
                bytes: bytes.to_vec(),
            }]
        };

        Ok(ir::Module {
            types,
            imports,
            memory: Some(ir::Memory {
                min: memory_pages(&pool)?,
                max: None,
            }),
            globals,
            funcs,
            data,
            exports: vec![ir::Export {
                name: ENTRY.to_string(),
                index: user_base + ast::Program::SCRIPT.0,
            }],
        })
    }

    /// What a function is called in the `name` custom section. The script is
    /// the export name; an anonymous function expression is named after where
    /// it was written, which is the only thing that tells two of them apart.
    fn debug_name(program: &ast::Program, id: ast::FuncId) -> String {
        if id == ast::Program::SCRIPT {
            return ENTRY.to_string();
        }
        match &program.func(id).name {
            Some(name) => name.clone(),
            None => format!("<anonymous@{}>", program.func(id).span.offset()),
        }
    }

    /// `n` JS values, flattened into wasm value types. Every function in a
    /// compiled module speaks in these and nothing else.
    fn values(n: u32) -> Vec<ValType> {
        (0..n).flat_map(|_| repr::SLOTS).collect()
    }

    fn intern(types: &mut Vec<ir::FuncType>, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        let wanted = ir::FuncType {
            params: params.into_iter().map(ir::ValType::from).collect(),
            results: results.into_iter().map(ir::ValType::from).collect(),
        };
        if let Some(index) = types.iter().position(|t| *t == wanted) {
            return index as u32;
        }
        types.push(wanted);
        (types.len() - 1) as u32
    }

    // -- the bridge from `repr`'s duplicate instruction set ------------------
    //
    // `repr.rs` declares its own copy of `ir::m1::Ins`, because it landed
    // before `ir.rs` could name an `i64`, and its header says the definition
    // belongs in `ir.rs` -- where it now is. Until `repr.rs` and `runtime.rs`
    // can be pointed at it, this is the single place the two meet, and it is
    // total in both directions by construction: the two enums have the same
    // variants. Retiring it is one `use` line in each of those two files and
    // deleting everything between here and the next section.

    impl From<repr::ValType> for ir::ValType {
        fn from(ty: repr::ValType) -> Self {
            match ty {
                repr::ValType::I32 => ir::ValType::I32,
                repr::ValType::I64 => ir::ValType::I64,
                repr::ValType::F64 => ir::ValType::F64,
            }
        }
    }

    impl From<BlockType> for ir::BlockType {
        fn from(ty: BlockType) -> Self {
            match ty {
                BlockType::Empty => ir::BlockType::Empty,
            }
        }
    }

    impl From<Ins> for ir::Ins {
        fn from(ins: Ins) -> Self {
            match ins {
                Ins::Block(ty) => ir::Ins::Block(ty.into()),
                Ins::Loop(ty) => ir::Ins::Loop(ty.into()),
                Ins::If(ty) => ir::Ins::If(ty.into()),
                Ins::End => ir::Ins::End,
                Ins::Br(depth) => ir::Ins::Br(depth),
                Ins::BrIf(depth) => ir::Ins::BrIf(depth),
                Ins::Return => ir::Ins::Return,
                Ins::Call(index) => ir::Ins::Call(index),
                Ins::Unreachable => ir::Ins::Unreachable,
                Ins::Drop => ir::Ins::Drop,
                Ins::LocalGet(i) => ir::Ins::LocalGet(i),
                Ins::LocalSet(i) => ir::Ins::LocalSet(i),
                Ins::LocalTee(i) => ir::Ins::LocalTee(i),
                Ins::GlobalGet(i) => ir::Ins::GlobalGet(i),
                Ins::GlobalSet(i) => ir::Ins::GlobalSet(i),
                Ins::I32Load(a, o) => ir::Ins::I32Load(a, o),
                Ins::I32Load8U(a, o) => ir::Ins::I32Load8U(a, o),
                Ins::I32Store(a, o) => ir::Ins::I32Store(a, o),
                Ins::I32Store8(a, o) => ir::Ins::I32Store8(a, o),
                Ins::I64Load(a, o) => ir::Ins::I64Load(a, o),
                Ins::I64Store(a, o) => ir::Ins::I64Store(a, o),
                Ins::MemorySize => ir::Ins::MemorySize,
                Ins::MemoryGrow => ir::Ins::MemoryGrow,
                Ins::I32Const(v) => ir::Ins::I32Const(v),
                Ins::I64Const(v) => ir::Ins::I64Const(v),
                Ins::F64Const(v) => ir::Ins::F64Const(v),
                Ins::I32Eqz => ir::Ins::I32Eqz,
                Ins::I32Eq => ir::Ins::I32Eq,
                Ins::I32Ne => ir::Ins::I32Ne,
                Ins::I32LtS => ir::Ins::I32LtS,
                Ins::I32LtU => ir::Ins::I32LtU,
                Ins::I32GeU => ir::Ins::I32GeU,
                Ins::I32Add => ir::Ins::I32Add,
                Ins::I32Sub => ir::Ins::I32Sub,
                Ins::I32Mul => ir::Ins::I32Mul,
                Ins::I32DivS => ir::Ins::I32DivS,
                Ins::I32RemS => ir::Ins::I32RemS,
                Ins::I32And => ir::Ins::I32And,
                Ins::I32Or => ir::Ins::I32Or,
                Ins::I32Shl => ir::Ins::I32Shl,
                Ins::I64Eq => ir::Ins::I64Eq,
                Ins::F64Eq => ir::Ins::F64Eq,
                Ins::F64Ne => ir::Ins::F64Ne,
                Ins::F64Lt => ir::Ins::F64Lt,
                Ins::F64Gt => ir::Ins::F64Gt,
                Ins::F64Le => ir::Ins::F64Le,
                Ins::F64Ge => ir::Ins::F64Ge,
                Ins::F64Abs => ir::Ins::F64Abs,
                Ins::F64Neg => ir::Ins::F64Neg,
                Ins::F64Add => ir::Ins::F64Add,
                Ins::F64Sub => ir::Ins::F64Sub,
                Ins::F64Mul => ir::Ins::F64Mul,
                Ins::F64Div => ir::Ins::F64Div,
                Ins::F64Copysign => ir::Ins::F64Copysign,
                Ins::F64Trunc => ir::Ins::F64Trunc,
                Ins::I32TruncF64S => ir::Ins::I32TruncF64S,
                Ins::I32WrapI64 => ir::Ins::I32WrapI64,
                Ins::I64ExtendI32U => ir::Ins::I64ExtendI32U,
                Ins::F64ConvertI32S => ir::Ins::F64ConvertI32S,
                Ins::F64ReinterpretI64 => ir::Ins::F64ReinterpretI64,
                Ins::I64ReinterpretF64 => ir::Ins::I64ReinterpretF64,
            }
        }
    }

    /// One built function, moved from `repr`'s vocabulary into the IR's.
    fn func(
        name: String,
        type_index: u32,
        locals: Vec<(u32, repr::ValType)>,
        body: Vec<Ins>,
    ) -> ir::Func {
        ir::Func {
            name: Some(name),
            type_index,
            locals: locals
                .into_iter()
                .map(|(count, ty)| (count, ir::ValType::from(ty)))
                .collect(),
            body: body.into_iter().map(ir::Ins::from).collect(),
        }
    }

    // -- the import table ----------------------------------------------------

    // -- the declared host table ---------------------------------------------

    /// Resolve every host name the script uses against the embedder's
    /// declarations, in **declaration order**.
    ///
    /// Declaration order and not use order: an embedder reading its own table
    /// can then predict the import list without reading the script. Only the
    /// declarations a script actually uses become imports, so a host is never
    /// asked to bind a capability the guest cannot reach.
    ///
    /// Everything this checks is a mismatch between a script and a table, or
    /// inside a table, so every rejection is a [`host_table`] one.
    fn bind(decls: &[HostFn], scan: &Scan) -> Result<Vec<Bound>, CompileError> {
        for (position, decl) in decls.iter().enumerate() {
            if decls[..position].iter().any(|d| d.name == decl.name) {
                return Err(host_table(
                    &format!("was given two host functions both named `{}`", decl.name),
                    0,
                ));
            }
            if let HostResult::Bytes { length } = &decl.result
                && *length == decl.field
            {
                return Err(host_table(
                    &format!(
                        "cannot import `{}.{}` as both the length pass and the read pass of `{}`; the two have different signatures, so they cannot be one import",
                        decl.module, decl.field, decl.name
                    ),
                    0,
                ));
            }
        }

        // Every name the script used has to be one of them, with the arity
        // the declaration names. `scan` already refused a name used at two
        // arities, so one number per name is the whole story.
        for (name, use_) in &scan.hosts {
            let Some(decl) = decls.iter().find(|d| d.name == *name) else {
                let known: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
                return Err(host_table(
                    &format!(
                        "has no host function named `{name}`; this embedder declares {}",
                        list(&known)
                    ),
                    scan.at(name),
                ));
            };
            let want = decl.params.len() as u32;
            if use_.arity != want {
                return Err(host_table(
                    &format!(
                        "was given the host function `{name}` with {want} argument(s), and this call passes {}",
                        use_.arity
                    ),
                    scan.at(name),
                ));
            }
        }

        let mut next = 0u32;
        let mut bound = Vec::new();
        for decl in decls {
            if !scan.hosts.contains_key(&decl.name) {
                continue;
            }
            // The length import comes first, so a reader of the import table
            // meets the two passes of a `Bytes` door in the order the
            // lowering calls them.
            let length = matches!(decl.result, HostResult::Bytes { .. }).then(|| {
                let at = next;
                next += 1;
                at
            });
            let index = next;
            next += 1;
            bound.push(Bound {
                decl: decl.clone(),
                length,
                index,
            });
        }
        Ok(bound)
    }

    /// `` `a`, `b` and `c` `` -- or "no host functions at all", which is the
    /// answer a reader of an empty table most needs.
    fn list(names: &[&str]) -> String {
        match names {
            [] => "no host functions at all".to_string(),
            [only] => format!("`{only}`"),
            [rest @ .., last] => {
                let rest: Vec<String> = rest.iter().map(|n| format!("`{n}`")).collect();
                format!("{} and `{last}`", rest.join(", "))
            }
        }
    }

    /// The import entries a declared table produces, in the order [`bind`]
    /// assigned their indices.
    fn raw_imports(bound: &[Bound], types: &mut Vec<ir::FuncType>) -> Vec<ir::Import> {
        let mut out = Vec::new();
        for b in bound {
            let params: Vec<ValType> = b
                .decl
                .params
                .iter()
                .flat_map(|p| match p {
                    HostParam::StrPtrLen => vec![ValType::I32, ValType::I32],
                    HostParam::I32 => vec![ValType::I32],
                    HostParam::F64 => vec![ValType::F64],
                })
                .collect();
            if let HostResult::Bytes { length } = &b.decl.result {
                out.push(ir::Import {
                    module: b.decl.module.clone(),
                    name: length.clone(),
                    type_index: intern(types, params.clone(), vec![ValType::I32]),
                });
            }
            let (extra, results) = match &b.decl.result {
                HostResult::Void => (Vec::new(), Vec::new()),
                HostResult::I32 => (Vec::new(), vec![ValType::I32]),
                HostResult::F64 => (Vec::new(), vec![ValType::F64]),
                // The read pass takes the destination and its capacity after
                // whatever the declaration names, and answers with how many
                // bytes it wrote.
                HostResult::Bytes { .. } => (vec![ValType::I32, ValType::I32], vec![ValType::I32]),
            };
            out.push(ir::Import {
                module: b.decl.module.clone(),
                name: b.decl.field.clone(),
                type_index: intern(types, [params, extra].concat(), results),
            });
        }
        out
    }

    /// What one walk of the program tells the module builder, before a single
    /// instruction is emitted.
    ///
    /// One walk and not two: both facts are properties of every expression in
    /// the program, and the second reader would be a second chance to
    /// disagree with the first about which expressions those are.
    #[derive(Debug, Default)]
    struct Scan {
        /// Every host name the program calls, sorted and deduplicated.
        ///
        /// A wasm import has one signature, so a name used at two arities has
        /// no single import to be. Overloading it would need a third world.
        hosts: BTreeMap<String, Use>,
        /// Whether the program writes `typeof` anywhere. The five strings
        /// 13.5.3 answers with are data-segment literals, so a program that
        /// never asks should not carry them -- see [`runtime::TypeNames`].
        type_of: bool,
        /// Whether the program writes a *computed* member access, `o[e]`. A
        /// dotted `o.a` does not count: ECMA-262 13.3.2.1 takes the String
        /// value of the IdentifierName and never runs ToPropertyKey, so a
        /// program with only dotted access needs none of the three constant
        /// answers -- see [`runtime::KeyNames`].
        computed_key: bool,
    }

    /// How one host name is used: the argument count every occurrence agrees
    /// on, and where the first of them is, so a diagnostic about the name can
    /// point at the script rather than at byte 0.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Use {
        arity: u32,
        at: usize,
    }

    impl Scan {
        fn hosts(&self) -> Vec<Host> {
            self.hosts
                .iter()
                .map(|(name, use_)| Host {
                    name: name.clone(),
                    arity: use_.arity,
                })
                .collect()
        }

        /// Where this name is first used, for a diagnostic about it.
        fn at(&self, name: &str) -> usize {
            self.hosts.get(name).map_or(0, |use_| use_.at)
        }
    }

    fn scan(program: &ast::Program) -> Result<Scan, CompileError> {
        let mut out = Scan::default();
        for function in &program.functions {
            for stmt in &function.body {
                host_stmt(stmt, &mut out)?;
            }
        }
        Ok(out)
    }

    fn note_host(
        scan: &mut Scan,
        name: &str,
        arity: u32,
        span: ast::Span,
    ) -> Result<(), CompileError> {
        match scan.hosts.get(name) {
            Some(known) if known.arity != arity => Err(unsupported(
                Boundary::ThirdBinding,
                &format!("calling the host name `{name}` with two different argument counts"),
                span.offset(),
            )),
            // The *first* occurrence is the one a diagnostic points at, so a
            // repeat does not move it.
            Some(_) => Ok(()),
            None => {
                scan.hosts.insert(
                    name.to_string(),
                    Use {
                        arity,
                        at: span.offset(),
                    },
                );
                Ok(())
            }
        }
    }

    fn host_stmt(stmt: &ast::Stmt, scan: &mut Scan) -> Result<(), CompileError> {
        match &stmt.kind {
            ast::StmtKind::Empty | ast::StmtKind::Func { .. } => Ok(()),
            ast::StmtKind::Expr(e) => host_expr(e, scan),
            ast::StmtKind::Decl(declarators) => declarators
                .iter()
                .filter_map(|d| d.init.as_ref())
                .try_for_each(|e| host_expr(e, scan)),
            ast::StmtKind::Block(stmts) => stmts.iter().try_for_each(|s| host_stmt(s, scan)),
            ast::StmtKind::If { test, then, alt } => {
                host_expr(test, scan)?;
                host_stmt(then, scan)?;
                alt.iter().try_for_each(|s| host_stmt(s, scan))
            }
            ast::StmtKind::While { test, body } => {
                host_expr(test, scan)?;
                host_stmt(body, scan)
            }
            ast::StmtKind::For {
                init,
                test,
                update,
                body,
            } => {
                init.iter().try_for_each(|s| host_stmt(s, scan))?;
                test.iter().try_for_each(|e| host_expr(e, scan))?;
                update.iter().try_for_each(|e| host_expr(e, scan))?;
                host_stmt(body, scan)
            }
            ast::StmtKind::Return(value) => value.iter().try_for_each(|e| host_expr(e, scan)),
        }
    }

    fn host_expr(expr: &ast::Expr, scan: &mut Scan) -> Result<(), CompileError> {
        match &expr.kind {
            ast::ExprKind::Int(_)
            | ast::ExprKind::Str(_)
            | ast::ExprKind::Bool(_)
            | ast::ExprKind::Null
            | ast::ExprKind::Undefined
            | ast::ExprKind::Arg(_)
            | ast::ExprKind::Function(_) => Ok(()),
            // A bare host name is a zero-argument call, as it is at M0.
            ast::ExprKind::Name(name) => match &name.res {
                ast::Res::Host(text) => note_host(scan, text, 0, expr.span),
                _ => Ok(()),
            },
            ast::ExprKind::Call { callee, args } => {
                // The callee of a host call is the call, not a use of the
                // name on its own, so it is not walked as one.
                match &callee.kind {
                    ast::ExprKind::Name(name) => match &name.res {
                        ast::Res::Host(text) => {
                            note_host(scan, text, args.len() as u32, expr.span)?;
                        }
                        _ => host_expr(callee, scan)?,
                    },
                    _ => host_expr(callee, scan)?,
                }
                args.iter().try_for_each(|a| host_expr(a, scan))
            }
            ast::ExprKind::Object(properties) => properties
                .iter()
                .try_for_each(|property| host_expr(&property.value, scan)),
            ast::ExprKind::Member { object, key } => {
                host_expr(object, scan)?;
                match key {
                    ast::MemberKey::Static(_) => Ok(()),
                    ast::MemberKey::Computed(key) => {
                        scan.computed_key = true;
                        host_expr(key, scan)
                    }
                }
            }
            ast::ExprKind::Unary(op, operand) => {
                scan.type_of |= *op == ast::UnaryOp::TypeOf;
                host_expr(operand, scan)
            }
            ast::ExprKind::Update { target, .. } => host_expr(target, scan),
            ast::ExprKind::Binary(_, lhs, rhs) | ast::ExprKind::Logical(_, lhs, rhs) => {
                host_expr(lhs, scan)?;
                host_expr(rhs, scan)
            }
            ast::ExprKind::Assign { target, value, .. } => {
                host_expr(target, scan)?;
                host_expr(value, scan)
            }
        }
    }

    // -- one function --------------------------------------------------------

    /// Where a binding's two words live.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Place {
        /// Base index of a pair of locals.
        Local(u32),
        /// Base index of a pair of globals.
        Global(u32),
    }

    /// What an assignment or an update writes to: ECMA-262's ~simple~
    /// AssignmentTargetType, in the two forms 13.15.1 gives it here.
    ///
    /// A property target is not a *place*, which is why this exists beside
    /// [`Place`] rather than as another variant of it: reading and writing one
    /// are calls, and the receiver and the key have to be evaluated exactly
    /// once and then held. They are held in scratch locals, taken by
    /// [`Lower::target`] and given back by [`Lower::release`], which is what
    /// makes `o.a += f()` evaluate `o` once and in the right order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Target {
        Binding(Place),
        Member {
            /// Base of the value local holding the receiver.
            object: u32,
            /// The raw `i32` local holding the key's string record.
            key: u32,
        },
    }

    struct Lower<'a> {
        program: &'a ast::Program,
        ctx: &'a Ctx,
        pool: &'a mut StringPool,
        table: &'a Table,
        user_base: u32,
        id: ast::FuncId,
        f: FnBuild,
        /// The script's completion value, per ECMA-262 -- see [`Lower::stmt`].
        /// `None` in every other function, where only `return` produces one.
        completion: Option<u32>,
        /// Scratch value locals not currently held by an expression. Lowering
        /// nests, so taking and giving back is a stack and one local can serve
        /// many sites.
        free: Vec<u32>,
        /// The same, for the bare `i32` locals a declared host call needs.
        /// Separate because a JS value is two words and these are one, so one
        /// pool cannot serve both.
        free_raw: Vec<u32>,
    }

    impl<'a> Lower<'a> {
        fn new(
            program: &'a ast::Program,
            ctx: &'a Ctx,
            pool: &'a mut StringPool,
            table: &'a Table,
            user_base: u32,
            id: ast::FuncId,
        ) -> Self {
            let function = program.func(id);
            let arity = if id == ast::Program::SCRIPT {
                program.arg_count
            } else {
                function.params.len() as u32
            };
            // Two front-end invariants this indexing rests on, and neither is
            // visible from the type: the script's arguments are `$N` rather
            // than bindings, and a function's parameters are the first
            // entries of its `bindings` with the slots to match.
            debug_assert!(
                id != ast::Program::SCRIPT || function.params.is_empty(),
                "the script's parameters are its `$N`, not bindings"
            );
            debug_assert!(
                function
                    .params
                    .iter()
                    .enumerate()
                    .all(|(position, binding)| program.binding(*binding).slot == position as u32),
                "a function's parameters must be the first of its bindings, in order"
            );
            let mut f = FnBuild::new(arity * WIDTH);
            // The bindings this function owns, minus its parameters, in slot
            // order: that is what makes `slot * WIDTH` the local index for a
            // parameter and a body binding alike. The script's bindings are
            // globals, so it declares none of them here.
            if id != ast::Program::SCRIPT {
                for _ in function.params.len()..function.bindings.len() {
                    f.value_local();
                }
            }
            let completion = (id == ast::Program::SCRIPT).then(|| f.value_local());
            Lower {
                program,
                ctx,
                pool,
                table,
                user_base,
                id,
                f,
                completion,
                free: Vec::new(),
                free_raw: Vec::new(),
            }
        }

        fn function(mut self) -> Result<FnBuild, CompileError> {
            if self.id == ast::Program::SCRIPT {
                // The fault word describes *this* call, so the entry point
                // clears it before anything can write one. Without this a
                // heap exhaustion recorded by an earlier call would still be
                // sitting there when a later call trapped for its own,
                // entirely different reason.
                runtime::clear_fault(&mut self.f.body);
            }
            for stmt in &self.program.func(self.id).body {
                self.stmt(stmt)?;
            }
            // Falling off the end. The script yields its completion value --
            // which is `undefined` unless a statement produced one, and is
            // already `undefined` because a fresh local is zeroed and
            // `TAG_UNDEFINED` is 0. Any other function yields `undefined`.
            match self.completion {
                Some(base) => load_local(base, &mut self.f.body),
                None => const_undefined(&mut self.f.body),
            }
            Ok(self.f)
        }

        // -- scratch ---------------------------------------------------------

        fn take(&mut self) -> u32 {
            self.free.pop().unwrap_or_else(|| self.f.value_local())
        }

        fn give(&mut self, base: u32) {
            self.free.push(base);
        }

        fn take_raw(&mut self) -> u32 {
            self.free_raw
                .pop()
                .unwrap_or_else(|| self.f.local(ValType::I32))
        }

        fn give_raw(&mut self, index: u32) {
            self.free_raw.push(index);
        }

        fn push(&mut self, ins: Ins) {
            self.f.body.push(ins);
        }

        /// Run `build` with a fresh instruction buffer and hand back what it
        /// emitted. `repr`'s constructors take their payload as a built run
        /// rather than expecting it on the stack, and this is how an arbitrary
        /// sub-expression becomes one.
        fn detached(
            &mut self,
            build: impl FnOnce(&mut Self) -> Result<(), CompileError>,
        ) -> Result<Vec<Ins>, CompileError> {
            let saved = std::mem::take(&mut self.f.body);
            let outcome = build(self);
            let inner = std::mem::replace(&mut self.f.body, saved);
            outcome.map(|()| inner)
        }

        // -- storage ---------------------------------------------------------

        fn place(&self, id: ast::BindingId) -> Place {
            let binding = self.program.binding(id);
            if binding.func == ast::Program::SCRIPT {
                Place::Global(BINDING_GLOBALS + binding.slot * WIDTH)
            } else {
                Place::Local(binding.slot * WIDTH)
            }
        }

        fn load(&mut self, place: Place) {
            match place {
                Place::Local(base) => load_local(base, &mut self.f.body),
                Place::Global(base) => {
                    for k in 0..WIDTH {
                        self.push(Ins::GlobalGet(base + k));
                    }
                }
            }
        }

        fn store(&mut self, place: Place) {
            match place {
                Place::Local(base) => store_local(base, &mut self.f.body),
                Place::Global(base) => {
                    // The payload is on top, so the words come off backwards.
                    for k in (0..WIDTH).rev() {
                        self.push(Ins::GlobalSet(base + k));
                    }
                }
            }
        }

        // -- statements ------------------------------------------------------

        /// Every statement is stack-neutral. The script also maintains its
        /// completion value (ECMA-262 14.1.1 and the `UpdateEmpty` in 14.6.7,
        /// 14.7.1.1): an expression statement sets it, a declaration and a
        /// block leave it alone, and `if`/`while`/`for` reset it to
        /// `undefined` before their body runs -- which is exactly what
        /// `UpdateEmpty(V, undefined)` says, and is why `1; if (false) { 2; }`
        /// is `undefined` and not `1`.
        fn stmt(&mut self, stmt: &ast::Stmt) -> Result<(), CompileError> {
            match &stmt.kind {
                ast::StmtKind::Empty => Ok(()),
                ast::StmtKind::Expr(expr) => {
                    self.expr(expr)?;
                    match self.completion {
                        Some(base) => store_local(base, &mut self.f.body),
                        None => drop_value(&mut self.f.body),
                    }
                    Ok(())
                }
                ast::StmtKind::Decl(declarators) => {
                    for declarator in declarators {
                        self.declarator(declarator)?;
                    }
                    Ok(())
                }
                // No wasm block: this milestone has no `break` or `continue`,
                // so nothing can branch to a block's label, and the front end
                // has already turned block scoping into distinct bindings.
                ast::StmtKind::Block(stmts) => stmts.iter().try_for_each(|s| self.stmt(s)),
                ast::StmtKind::If { test, then, alt } => self.if_stmt(test, then, alt.as_deref()),
                ast::StmtKind::While { test, body } => self.loop_stmt(Some(test), None, body),
                ast::StmtKind::For {
                    init,
                    test,
                    update,
                    body,
                } => {
                    if let Some(init) = init {
                        self.stmt(init)?;
                    }
                    self.loop_stmt(test.as_ref(), update.as_ref(), body)
                }
                ast::StmtKind::Return(value) => {
                    match value {
                        Some(expr) => self.expr(expr)?,
                        None => const_undefined(&mut self.f.body),
                    }
                    self.push(Ins::Return);
                    Ok(())
                }
                // Hoisted before the program was walked: a declaration binds a
                // name to a function index, and a function index needs no
                // storage and nothing to run.
                ast::StmtKind::Func { .. } => Ok(()),
            }
        }

        fn declarator(&mut self, declarator: &ast::Declarator) -> Result<(), CompileError> {
            let binding = self.program.binding(declarator.binding);
            match (&declarator.init, binding.kind) {
                // `const f = function () {}`: the name is the function, and
                // the function is an index, so there is nothing to store.
                (
                    Some(ast::Expr {
                        kind: ast::ExprKind::Function(_),
                        ..
                    }),
                    ast::BindingKind::Function(_),
                ) => Ok(()),
                (Some(init), _) => {
                    let place = self.place(declarator.binding);
                    self.expr(init)?;
                    self.store(place);
                    Ok(())
                }
                // `let x;` binds a *fresh* `undefined` every time it runs,
                // which matters inside a loop, where the storage is the same
                // local on the second pass. `var x;` with no initialiser is a
                // runtime no-op (ECMA-262 14.3.2.1), so it does not.
                (None, ast::BindingKind::Var) => Ok(()),
                (None, _) => {
                    let place = self.place(declarator.binding);
                    const_undefined(&mut self.f.body);
                    self.store(place);
                    Ok(())
                }
            }
        }

        fn if_stmt(
            &mut self,
            test: &ast::Expr,
            then: &ast::Stmt,
            alt: Option<&ast::Stmt>,
        ) -> Result<(), CompileError> {
            self.reset_completion();
            match alt {
                // No `else`: one `if` block, entered when the test is truthy.
                None => {
                    self.truthy(test)?;
                    self.push(Ins::If(BlockType::Empty));
                    self.stmt(then)?;
                    self.push(Ins::End);
                }
                // With `else`: the two-block form in this module's header. The
                // inner `br_if 0` skips the then-arm; the `br 1` at its end
                // skips the else-arm. Both depths are the ones opened here.
                Some(alt) => {
                    self.push(Ins::Block(BlockType::Empty));
                    self.push(Ins::Block(BlockType::Empty));
                    self.truthy(test)?;
                    self.push(Ins::I32Eqz);
                    self.push(Ins::BrIf(0));
                    self.stmt(then)?;
                    self.push(Ins::Br(1));
                    self.push(Ins::End);
                    self.stmt(alt)?;
                    self.push(Ins::End);
                }
            }
            Ok(())
        }

        /// `while` and `for` are the same shape; `for`'s `init` has already
        /// run and its `update` is the only difference.
        ///
        /// ```text
        /// block                  ;; the exit label, depth 1 from the body
        ///   loop                 ;; the back edge, depth 0
        ///     <test>; i32.eqz; br_if 1
        ///     <body>
        ///     <update>
        ///     br 0
        ///   end
        /// end
        /// ```
        fn loop_stmt(
            &mut self,
            test: Option<&ast::Expr>,
            update: Option<&ast::Expr>,
            body: &ast::Stmt,
        ) -> Result<(), CompileError> {
            self.reset_completion();
            self.push(Ins::Block(BlockType::Empty));
            self.push(Ins::Loop(BlockType::Empty));
            // A missing test is `true`, which is a loop with no exit edge
            // other than a `return` inside it.
            if let Some(test) = test {
                self.truthy(test)?;
                self.push(Ins::I32Eqz);
                self.push(Ins::BrIf(1));
            }
            self.stmt(body)?;
            if let Some(update) = update {
                self.expr(update)?;
                drop_value(&mut self.f.body);
            }
            self.push(Ins::Br(0));
            self.push(Ins::End);
            self.push(Ins::End);
            Ok(())
        }

        /// `UpdateEmpty(V, undefined)`: a control-flow statement whose body
        /// produces nothing still produces `undefined`.
        fn reset_completion(&mut self) {
            if let Some(base) = self.completion {
                const_undefined(&mut self.f.body);
                store_local(base, &mut self.f.body);
            }
        }

        /// Leave one `i32` on the stack: ToBoolean of the expression.
        fn truthy(&mut self, test: &ast::Expr) -> Result<(), CompileError> {
            self.expr(test)?;
            let call = self.ctx.call(Rt::Truthy);
            self.push(call);
            Ok(())
        }

        // -- expressions -----------------------------------------------------

        /// Leave exactly one JS value -- two wasm values -- on the stack.
        fn expr(&mut self, expr: &ast::Expr) -> Result<(), CompileError> {
            match &expr.kind {
                // An integer literal is a Number: ECMA-262 6.1.6.1 has one
                // numeric type and it is the double. `1/2` is `0.5` here.
                ast::ExprKind::Int(value) => {
                    const_number(f64::from(*value), &mut self.f.body);
                    Ok(())
                }
                ast::ExprKind::Str(text) => {
                    let pointer = self.pool.intern(text);
                    const_string(pointer, &mut self.f.body);
                    Ok(())
                }
                ast::ExprKind::Bool(value) => {
                    const_bool(*value, &mut self.f.body);
                    Ok(())
                }
                ast::ExprKind::Null => {
                    const_null(&mut self.f.body);
                    Ok(())
                }
                ast::ExprKind::Undefined => {
                    const_undefined(&mut self.f.body);
                    Ok(())
                }
                // `$N` is the script's Nth parameter, and the front end has
                // already refused one inside a nested function.
                ast::ExprKind::Arg(index) => {
                    load_local(index * WIDTH, &mut self.f.body);
                    Ok(())
                }
                ast::ExprKind::Name(name) => self.name(name, expr.span),
                ast::ExprKind::Object(properties) => self.object_literal(properties),
                // 13.3.2.1 and 13.3.3.1 both evaluate the object first and the
                // key second, which is exactly the order the two words and the
                // one word have to reach `__obj_get` in -- so a member read
                // needs no scratch local at all.
                ast::ExprKind::Member { object, key } => {
                    self.expr(object)?;
                    self.key(key)?;
                    let call = self.ctx.call(Rt::ObjGet);
                    self.push(call);
                    Ok(())
                }
                ast::ExprKind::Function(_) => Err(unsupported(
                    Boundary::FullJs,
                    "using a function as a value",
                    expr.span.offset(),
                )),
                ast::ExprKind::Call { callee, args } => self.call(callee, args, expr.span),
                ast::ExprKind::Unary(op, operand) => self.unary(*op, operand),
                ast::ExprKind::Update { op, prefix, target } => self.update(*op, *prefix, target),
                ast::ExprKind::Binary(op, lhs, rhs) => {
                    self.expr(lhs)?;
                    self.expr(rhs)?;
                    let call = self.ctx.call(binary(*op));
                    self.push(call);
                    Ok(())
                }
                ast::ExprKind::Logical(op, lhs, rhs) => self.logical(*op, lhs, rhs),
                ast::ExprKind::Assign { op, target, value } => self.assign(*op, target, value),
            }
        }

        /// An ObjectLiteral, ECMA-262 13.2.5.5: a fresh object, then one
        /// CreateDataPropertyOrThrow per PropertyDefinition, in source order.
        ///
        /// The properties go in through `__obj_set` rather than being written
        /// straight into the record. That is a deliberate cost -- a scan of
        /// what is already there, per property -- and it buys the one thing a
        /// straight write would get wrong: `{ a: 1, a: 2 }` is *one* property
        /// written twice, because 13.2.5.5 evaluates each definition in turn
        /// against the same object. A literal that filled slots blindly would
        /// produce two entries with the same key, and the second would be
        /// unreachable by every later read.
        ///
        /// The record is sized to the property count, so a literal never
        /// reallocates -- and a duplicate key leaves one slot unused, which is
        /// the whole price of the rule above.
        fn object_literal(&mut self, properties: &[ast::Property]) -> Result<(), CompileError> {
            let slot = self.take();
            let new = self.ctx.call(Rt::ObjNew);
            let count = properties.len() as i32;
            box_object(&[Ins::I32Const(count), new], &mut self.f.body);
            store_local(slot, &mut self.f.body);
            let set = self.ctx.call(Rt::ObjSet);
            for property in properties {
                let key = self.pool.intern(&property.key);
                load_local(slot, &mut self.f.body);
                self.push(Ins::I32Const(key));
                self.expr(&property.value)?;
                self.push(set);
            }
            load_local(slot, &mut self.f.body);
            self.give(slot);
            Ok(())
        }

        /// Leave one `i32` on the stack: the string record naming the property.
        ///
        /// The two arms are ECMA-262's two, not a fast path and a slow one. A
        /// static key is 13.3.2.1, whose key is *the String value of the
        /// IdentifierName* -- there is no expression and no ToPropertyKey in
        /// that algorithm, so interning the literal is the whole of it. A
        /// computed key is 13.3.3.1, which evaluates, GetValues and then runs
        /// ToPropertyKey, and `__to_key` is that step.
        ///
        /// Written this way on purpose. The alternative -- one `Expr` key, and
        /// a lowering that recognises a string literal and skips `__to_key` --
        /// computes the same answers and is the shape `RESULTS.md` L2.5
        /// records as the disease: an exemption granted per call site because
        /// the compiler happens to know a type there. Following the grammar
        /// instead makes it a property of the *node*, which no later change
        /// can quietly widen.
        fn key(&mut self, key: &ast::MemberKey) -> Result<(), CompileError> {
            match key {
                ast::MemberKey::Static(name) => {
                    let pointer = self.pool.intern(name);
                    self.push(Ins::I32Const(pointer));
                }
                ast::MemberKey::Computed(expr) => {
                    self.expr(expr)?;
                    let call = self.ctx.call(Rt::ToKey);
                    self.push(call);
                }
            }
            Ok(())
        }

        /// Push one JS value: what the target currently holds.
        fn load_target(&mut self, target: &Target) {
            match *target {
                Target::Binding(place) => self.load(place),
                Target::Member { object, key } => {
                    load_local(object, &mut self.f.body);
                    self.push(Ins::LocalGet(key));
                    let call = self.ctx.call(Rt::ObjGet);
                    self.push(call);
                }
            }
        }

        /// Write the JS value held in the locals at `slot` into the target.
        ///
        /// The value comes from a local rather than from the stack, because
        /// `__obj_set` takes the receiver and the key *before* it -- and
        /// because both callers already hold the value in a local, the
        /// assignment's result being the value assigned.
        fn store_target(&mut self, target: &Target, slot: u32) {
            match *target {
                Target::Binding(place) => {
                    load_local(slot, &mut self.f.body);
                    self.store(place);
                }
                Target::Member { object, key } => {
                    load_local(object, &mut self.f.body);
                    self.push(Ins::LocalGet(key));
                    load_local(slot, &mut self.f.body);
                    let call = self.ctx.call(Rt::ObjSet);
                    self.push(call);
                }
            }
        }

        /// Give back the scratch a member target held, innermost first.
        fn release(&mut self, target: Target) {
            if let Target::Member { object, key } = target {
                self.give_raw(key);
                self.give(object);
            }
        }

        fn name(&mut self, name: &ast::Name, span: ast::Span) -> Result<(), CompileError> {
            match &name.res {
                ast::Res::Local(id) | ast::Res::Global(id) => {
                    let place = self.place(*id);
                    self.load(place);
                    Ok(())
                }
                // As at M0: a bare host name is the zero-argument call.
                ast::Res::Host(text) => self.host_call(text, &[]),
                // The front end refuses a read of a function binding, so a
                // `Callee` only ever reaches `call` below.
                ast::Res::Callee(_) => Err(unsupported(
                    Boundary::FullJs,
                    "using a function as a value",
                    span.offset(),
                )),
                ast::Res::Unresolved => {
                    unreachable!("the parser resolves every occurrence before it returns")
                }
            }
        }

        fn call(
            &mut self,
            callee: &ast::Expr,
            args: &[ast::Expr],
            span: ast::Span,
        ) -> Result<(), CompileError> {
            let target = match &callee.kind {
                ast::ExprKind::Name(ast::Name {
                    res: ast::Res::Host(text),
                    ..
                }) => return self.host_call(text, args),
                ast::ExprKind::Name(ast::Name {
                    res: ast::Res::Callee(id),
                    ..
                }) => match self.program.binding(*id).kind {
                    ast::BindingKind::Function(func) => func,
                    _ => unreachable!("the parser only classifies a function binding as a callee"),
                },
                // `(function () {})()`.
                ast::ExprKind::Function(func) => *func,
                _ => {
                    return Err(unsupported(
                        Boundary::FullJs,
                        "calling a value that is not a known function",
                        span.offset(),
                    ));
                }
            };
            let arity = self.program.func(target).params.len() as u32;
            self.arguments(args, arity)?;
            self.push(Ins::Call(self.user_base + target.0));
            Ok(())
        }

        fn host_call(&mut self, name: &str, args: &[ast::Expr]) -> Result<(), CompileError> {
            match self.table {
                Table::Pairs(hosts) => {
                    let index = hosts
                        .iter()
                        .position(|host| host.name == name)
                        .expect("every host name in the tree was collected");
                    let arity = hosts[index].arity;
                    self.arguments(args, arity)?;
                    self.push(Ins::Call(index as u32));
                    Ok(())
                }
                Table::Raw(bound) => {
                    let b = bound
                        .iter()
                        .find(|b| b.decl.name == name)
                        .expect("`bind` refused every name it could not resolve");
                    self.declared_call(&b.clone(), args)
                }
            }
        }

        /// A call across the raw door an embedder declared.
        ///
        /// ```text
        /// <evaluate each JS argument into a scratch pair, left to right>
        /// <unwrap the pairs onto the declared raw parameters>
        /// call the import
        /// <rewrap the raw result as a JS value>
        /// ```
        ///
        /// The arguments are evaluated *first*, all of them, and only then
        /// unwrapped. That is not a convenience: an argument can assign or
        /// call, JavaScript evaluates them left to right, and a
        /// [`HostResult::Bytes`] result pushes the raw parameters twice.
        /// Evaluating in place would reorder the first and repeat the second.
        fn declared_call(&mut self, b: &Bound, args: &[ast::Expr]) -> Result<(), CompileError> {
            debug_assert_eq!(
                args.len(),
                b.decl.params.len(),
                "`bind` checked the arity of every host name"
            );
            for (position, (arg, param)) in args.iter().zip(&b.decl.params).enumerate() {
                if let Some(got) = static_type(arg)
                    && got != param.wants()
                {
                    return Err(host_table(
                        &format!(
                            "cannot pass {got} to argument {} of the host function `{}`, which is declared to take {}",
                            position + 1,
                            b.decl.name,
                            param.wants()
                        ),
                        arg.span.offset(),
                    ));
                }
            }

            let mut slots = Vec::with_capacity(args.len());
            for arg in args {
                let slot = self.take();
                self.expr(arg)?;
                store_local(slot, &mut self.f.body);
                slots.push(slot);
            }

            match &b.decl.result {
                HostResult::Void => {
                    self.unwrap_args(&slots, &b.decl.params);
                    self.push(Ins::Call(b.index));
                    const_undefined(&mut self.f.body);
                }
                HostResult::I32 | HostResult::F64 => {
                    let widen = matches!(b.decl.result, HostResult::I32);
                    let params = b.decl.params.clone();
                    let index = b.index;
                    let inner = self.detached(|me| {
                        me.unwrap_args(&slots, &params);
                        me.push(Ins::Call(index));
                        if widen {
                            me.push(Ins::F64ConvertI32S);
                        }
                        Ok(())
                    })?;
                    box_number(&inner, &mut self.f.body);
                }
                HostResult::Bytes { .. } => self.two_pass_string(b, &slots),
            }

            for slot in slots.into_iter().rev() {
                self.give(slot);
            }
            Ok(())
        }

        /// Push the raw parameters the declaration names, reading each JS
        /// argument out of the scratch pair it was evaluated into.
        ///
        /// The type tests here are [`super::repr`]'s accessors, so a value of
        /// the wrong type traps rather than being reinterpreted. That is the
        /// runtime half of the policy whose compile-time half is
        /// [`static_type`] above: a dynamic language cannot settle every
        /// argument's type before it runs, and must not pretend to.
        fn unwrap_args(&mut self, slots: &[u32], params: &[HostParam]) {
            for (slot, param) in slots.iter().zip(params) {
                match param {
                    // The bytes, not the record: a host reading `(ptr, len)`
                    // wants the text, and the 4-byte length header in front of
                    // it is this engine's business.
                    HostParam::StrPtrLen => {
                        unbox_string(*slot, &mut self.f.body);
                        self.push(Ins::I32Const(STRING_HEADER));
                        self.push(Ins::I32Add);
                        unbox_string(*slot, &mut self.f.body);
                        self.push(Ins::I32Load(2, 0));
                    }
                    // The Number has to *be* an `i32`. `f64.trunc` rejects a
                    // fractional value and `i32.trunc_f64_s` rejects a NaN, an
                    // infinity and anything out of range -- so the host either
                    // receives the number the script wrote or nothing at all.
                    // Rounding here would hand a host a number no line of the
                    // script contains.
                    HostParam::I32 => {
                        let scratch = self.f.local(ValType::F64);
                        unbox_number(*slot, &mut self.f.body);
                        self.push(Ins::LocalTee(scratch));
                        self.push(Ins::F64Trunc);
                        self.push(Ins::LocalGet(scratch));
                        self.push(Ins::F64Ne);
                        self.push(Ins::If(BlockType::Empty));
                        self.push(Ins::Unreachable);
                        self.push(Ins::End);
                        self.push(Ins::LocalGet(scratch));
                        self.push(Ins::I32TruncF64S);
                    }
                    HostParam::F64 => unbox_number(*slot, &mut self.f.body),
                }
            }
        }

        /// [`HostResult::Bytes`]: ask the length, allocate a string record of
        /// that size on the engine's own heap, ask for the copy, and check it.
        ///
        /// The checks are the point, and there are two of them because a host
        /// can be wrong in two independent ways.
        ///
        /// The second one -- copied bytes against promised bytes -- catches a
        /// host that writes a different amount than it announced. On its own it
        /// is not enough: it compares one host answer to another host answer, so
        /// two *matching* wrong answers pass it. In particular `-1` is what a
        /// raw contract returns for "your buffer is too small", and a host that
        /// answers `-1` to both calls used to get a String whose length header
        /// read `0xFFFFFFFF` -- the fabricated tail this function exists to
        /// prevent -- and, because `__alloc` rounds with `(size + 3) & -4`,
        /// moved the bump pointer *backwards* over
        /// [`crate::runtime::FAULT_WORD`].
        ///
        /// So the first check asks the question the second one cannot: is the
        /// announced length a length at all. A negative answer is refused here,
        /// at the boundary the host lied across, before it becomes a size, a
        /// record header or an address.
        fn two_pass_string(&mut self, b: &Bound, slots: &[u32]) {
            let length = b.length.expect("a Bytes result binds a length import");
            let n = self.take_raw();
            let p = self.take_raw();
            let params = b.decl.params.clone();

            self.unwrap_args(slots, &params);
            self.push(Ins::Call(length));
            self.push(Ins::LocalSet(n));

            self.push(Ins::LocalGet(n));
            self.push(Ins::I32Const(0));
            self.push(Ins::I32LtS);
            self.push(Ins::If(BlockType::Empty));
            self.push(Ins::Unreachable);
            self.push(Ins::End);

            self.push(Ins::LocalGet(n));
            self.push(Ins::I32Const(STRING_HEADER));
            self.push(Ins::I32Add);
            let alloc = self.ctx.call(Rt::Alloc);
            self.push(alloc);
            self.push(Ins::LocalSet(p));
            self.push(Ins::LocalGet(p));
            self.push(Ins::LocalGet(n));
            self.push(Ins::I32Store(2, 0));

            self.unwrap_args(slots, &params);
            self.push(Ins::LocalGet(p));
            self.push(Ins::I32Const(STRING_HEADER));
            self.push(Ins::I32Add);
            self.push(Ins::LocalGet(n));
            self.push(Ins::Call(b.index));
            self.push(Ins::LocalGet(n));
            self.push(Ins::I32Ne);
            self.push(Ins::If(BlockType::Empty));
            self.push(Ins::Unreachable);
            self.push(Ins::End);

            box_string(&[Ins::LocalGet(p)], &mut self.f.body);
            self.give_raw(p);
            self.give_raw(n);
        }

        /// Reconcile a JavaScript argument list with a wasm one.
        ///
        /// wasm calls are arity-exact; JavaScript's are not. A missing
        /// argument is `undefined`, and a surplus one is still *evaluated* --
        /// it can assign, or call -- and only then discarded.
        fn arguments(&mut self, args: &[ast::Expr], arity: u32) -> Result<(), CompileError> {
            for arg in args {
                self.expr(arg)?;
            }
            for _ in arity as usize..args.len() {
                drop_value(&mut self.f.body);
            }
            for _ in args.len()..arity as usize {
                const_undefined(&mut self.f.body);
            }
            Ok(())
        }

        fn unary(&mut self, op: ast::UnaryOp, operand: &ast::Expr) -> Result<(), CompileError> {
            match op {
                ast::UnaryOp::Neg => {
                    self.expr(operand)?;
                    let call = self.ctx.call(Rt::Neg);
                    self.push(call);
                }
                // Unary `+` is ToNumber, not a no-op (ECMA-262 13.5.4).
                ast::UnaryOp::Plus => {
                    let to_number = self.ctx.call(Rt::ToNumber);
                    let inner = self.detached(|me| {
                        me.expr(operand)?;
                        me.push(to_number);
                        Ok(())
                    })?;
                    box_number(&inner, &mut self.f.body);
                }
                // 13.5.3, a String answer, so it goes through `__typeof`
                // rather than being folded here: the operand's type is not
                // known until it runs.
                ast::UnaryOp::TypeOf => {
                    self.expr(operand)?;
                    let call = self.ctx.call(Rt::TypeOf);
                    self.push(call);
                }
                ast::UnaryOp::Not => {
                    let truthy = self.ctx.call(Rt::Truthy);
                    let inner = self.detached(|me| {
                        me.expr(operand)?;
                        me.push(truthy);
                        me.push(Ins::I32Eqz);
                        Ok(())
                    })?;
                    box_bool(&inner, &mut self.f.body);
                }
            }
            Ok(())
        }

        /// `&&` and `||`, which yield an *operand* and evaluate the right one
        /// only if the left says so (ECMA-262 13.13).
        ///
        /// The result goes through a scratch local rather than a typed block,
        /// because `repr`'s `BlockType` has only `Empty`: a block that yields
        /// a JS value would need a multi-value block type, and a local costs
        /// two words once per site instead of a type-section entry.
        fn logical(
            &mut self,
            op: ast::LogicalOp,
            lhs: &ast::Expr,
            rhs: &ast::Expr,
        ) -> Result<(), CompileError> {
            let slot = self.take();
            self.expr(lhs)?;
            store_local(slot, &mut self.f.body);
            load_local(slot, &mut self.f.body);
            let truthy = self.ctx.call(Rt::Truthy);
            self.push(truthy);
            // `&&` takes the right operand when the left is truthy; `||` when
            // it is not.
            if op == ast::LogicalOp::Or {
                self.push(Ins::I32Eqz);
            }
            self.push(Ins::If(BlockType::Empty));
            self.expr(rhs)?;
            store_local(slot, &mut self.f.body);
            self.push(Ins::End);
            load_local(slot, &mut self.f.body);
            self.give(slot);
            Ok(())
        }

        /// `=` and its compound forms, ECMA-262 13.15.2.
        ///
        /// The target is evaluated *first* and whole -- for `o.a` that means
        /// the receiver and the key, into scratch -- and only then the value.
        /// That is the spec's order (step 1.a evaluates the LeftHandSide, step
        /// 1.d the right), and it is what makes `o[k()] = v()` call `k` before
        /// `v`. It is also what makes a compound assignment read and write one
        /// reference rather than evaluating `o` twice.
        fn assign(
            &mut self,
            op: Option<ast::BinaryOp>,
            target: &ast::Expr,
            value: &ast::Expr,
        ) -> Result<(), CompileError> {
            let target = self.target(target)?;
            let slot = self.take();
            match op {
                None => self.expr(value)?,
                Some(op) => {
                    self.load_target(&target);
                    self.expr(value)?;
                    let call = self.ctx.call(binary(op));
                    self.push(call);
                }
            }
            // The value of an assignment is the value assigned, so it is
            // stored once and read twice rather than duplicated on the stack:
            // wasm has no two-word `dup`.
            store_local(slot, &mut self.f.body);
            self.store_target(&target, slot);
            load_local(slot, &mut self.f.body);
            self.give(slot);
            self.release(target);
            Ok(())
        }

        /// `++` and `--` (ECMA-262 13.4). The old value is `ToNumeric` of the
        /// target, not the target, which is why `x = true; x++` leaves `2` and
        /// yields `1` rather than `true`.
        fn update(
            &mut self,
            op: ast::UpdateOp,
            prefix: bool,
            target: &ast::Expr,
        ) -> Result<(), CompileError> {
            let target = self.target(target)?;
            let old = self.take();
            let new = self.take();

            let to_number = self.ctx.call(Rt::ToNumber);
            let inner = self.detached(|me| {
                me.load_target(&target);
                me.push(to_number);
                Ok(())
            })?;
            box_number(&inner, &mut self.f.body);
            store_local(old, &mut self.f.body);

            load_local(old, &mut self.f.body);
            const_number(1.0, &mut self.f.body);
            let call = self.ctx.call(match op {
                ast::UpdateOp::Inc => Rt::Add,
                ast::UpdateOp::Dec => Rt::Sub,
            });
            self.push(call);
            store_local(new, &mut self.f.body);

            self.store_target(&target, new);
            // The one difference between the two spellings is which of the
            // two values the expression is.
            load_local(if prefix { new } else { old }, &mut self.f.body);
            self.give(new);
            self.give(old);
            self.release(target);
            Ok(())
        }

        /// Evaluate what an assignment or an update writes to, once.
        ///
        /// The front end guarantees the target is a name or a member
        /// expression, and that a name is neither a `const`, a function, nor a
        /// host import. A name needs nothing evaluated -- its storage is an
        /// index. A member expression needs both halves of its reference put
        /// somewhere the write can reach after the value has been computed,
        /// which is what the two scratch locals are; [`Lower::release`] gives
        /// them back.
        fn target(&mut self, target: &ast::Expr) -> Result<Target, CompileError> {
            match &target.kind {
                ast::ExprKind::Name(ast::Name {
                    res: ast::Res::Local(id) | ast::Res::Global(id),
                    ..
                }) => Ok(Target::Binding(self.place(*id))),
                ast::ExprKind::Member { object, key } => {
                    let slot = self.take();
                    self.expr(object)?;
                    store_local(slot, &mut self.f.body);
                    let raw = self.take_raw();
                    self.key(key)?;
                    self.push(Ins::LocalSet(raw));
                    Ok(Target::Member {
                        object: slot,
                        key: raw,
                    })
                }
                _ => unreachable!("the parser refuses every other assignment target"),
            }
        }
    }

    /// The ECMA-262 language type an expression is known to have without
    /// running it, named as it reads inside a diagnostic, or `None` when the
    /// compiler cannot settle it.
    ///
    /// `None` for most of the language, and that is correct rather than
    /// unfinished: in a dynamic language a name's type is a property of the
    /// run, not of the text. This settles the cases where the text *is* the
    /// answer -- a literal, and the unary operators whose result type does
    /// not depend on their operand -- so that `log(1)` is a diagnostic with a
    /// byte offset instead of a trap the author has to reproduce. Everything
    /// else is checked where the type is known, which is at run time; see
    /// [`Lower::unwrap_args`].
    ///
    /// Widening this is safe in one direction only. A new arm must be one
    /// whose answer is certain, because a wrong `Some` refuses a script that
    /// would have run.
    fn static_type(expr: &ast::Expr) -> Option<&'static str> {
        Some(match &expr.kind {
            ast::ExprKind::Int(_) => "a Number",
            ast::ExprKind::Str(_) => "a String",
            ast::ExprKind::Bool(_) => "a Boolean",
            ast::ExprKind::Object(_) => "an Object",
            ast::ExprKind::Null => "Null",
            ast::ExprKind::Undefined => "Undefined",
            // 13.5.4 and 13.5.5: both are ToNumber of the operand.
            ast::ExprKind::Unary(ast::UnaryOp::Plus | ast::UnaryOp::Neg, _) => "a Number",
            // 13.5.7: ToBoolean, negated.
            ast::ExprKind::Unary(ast::UnaryOp::Not, _) => "a Boolean",
            // 13.5.3: always one of five strings.
            ast::ExprKind::Unary(ast::UnaryOp::TypeOf, _) => "a String",
            // 13.4: ToNumeric, and this engine has no BigInt.
            ast::ExprKind::Update { .. } => "a Number",
            _ => return None,
        })
    }

    /// Which runtime function an operator is. Every one of them is a call:
    /// dispatching on the operand types is what a dynamic `+` means, and
    /// inlining it is an optimisation this compiler does not have.
    fn binary(op: ast::BinaryOp) -> Rt {
        match op {
            ast::BinaryOp::Add => Rt::Add,
            ast::BinaryOp::Sub => Rt::Sub,
            ast::BinaryOp::Mul => Rt::Mul,
            ast::BinaryOp::Div => Rt::Div,
            ast::BinaryOp::Rem => Rt::Rem,
            ast::BinaryOp::Lt => Rt::Lt,
            ast::BinaryOp::Le => Rt::Le,
            ast::BinaryOp::Gt => Rt::Gt,
            ast::BinaryOp::Ge => Rt::Ge,
            ast::BinaryOp::Eq => Rt::Eq,
            ast::BinaryOp::Ne => Rt::Ne,
            ast::BinaryOp::StrictEq => Rt::StrictEq,
            ast::BinaryOp::StrictNe => Rt::StrictNe,
        }
    }
}
