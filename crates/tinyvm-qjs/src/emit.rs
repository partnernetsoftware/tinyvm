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
///                 I+R..U  the script, then every nested function by FuncId
///                 U..     one adapter per function that became a value
/// global    index 0       the bump-allocation pointer
///                 1..     two globals per script binding
/// table     element 0     left null on purpose
///                 1..     the adapters, in the order the values asked for them
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
/// # Calling a value, and why there is an adapter
///
/// wasm MVP has no first-class function reference, so a function *value* is an
/// index into the module's funcref table and calling one is `call_indirect`.
/// `call_indirect` matches the callee's signature exactly (spec 4.4.8);
/// JavaScript's calls do not match anything -- a missing argument is
/// `undefined` (ECMA-262 8.6.1) and a surplus one is evaluated and dropped
/// (13.3.8.1). Those two facts cannot both be true of one instruction, so
/// something has to reconcile them, and the choice of *where* is the whole
/// design:
///
/// * **At the call site**, by dispatching on the callee's arity at run time.
///   Rejected: the arity would have to ride in the payload, and every call site
///   would grow one `call_indirect` per arity the module contains.
/// * **In the callee**, by giving every user function one wide signature.
///   Rejected: a single eight-parameter function anywhere in a script would
///   then make every zero-argument *direct* call push eight `undefined` pairs.
///   The cost of the new capability would land on code that never uses it.
/// * **In an adapter**, which is what this does. The table holds one adapter
///   per function that became a value, all of one uniform signature; the
///   adapter forwards as many arguments as its target declares and lets the
///   rest fall away. Direct calls are untouched, a script that makes no
///   function a value emits no table, no element segment and no adapter, and
///   the arity reconciliation lives in one place per function instead of one
///   place per call.
///
/// The uniform arity is a **bound**, not a measurement: the widest parameter
/// list in the program, or the widest indirect call site, whichever is more.
/// A conservative bound and not the exact maximum over table entries, because
/// the exact answer is only known after lowering and the call sites need it
/// during. What it costs when it is loose is a few `undefined` pairs at an
/// indirect call site; what an exact answer would cost is a second pass whose
/// disagreement with the first would be silent.
///
/// # Where the `end`s come from
///
/// Every branch this module emits used to target a label it opened itself, at
/// a nesting it knew statically. `try`/`catch` ended that: a `throw` five
/// blocks deep has to reach a handler opened by a statement that is not its
/// parent, which is exactly the non-local target `break` and `continue` would
/// need. So [`m1::Lower`] now keeps the label depth (`depth`) and two stacks
/// of open targets (`handlers`, `finalizers`), and every branch to something
/// this frame did not just open is `self.depth - target`. Nothing else about
/// the shapes below changed: an `if` and a loop still branch to their own
/// labels by a constant.
///
/// # Unwinding, and what it costs the program that never throws
///
/// tinyvm's core does not implement the wasm exception-handling proposal.
/// `crates/tinyvm/src/wasm.rs`'s opcode decoder has no arm for `try` (0x06),
/// `catch` (0x07), `throw` (0x08), `rethrow` (0x09) or `try_table` (0x1F) --
/// it ends at `_other => return Err(WasmError::Decode("unsupported opcode
/// 0x"))`, line 2931 -- and its section table ranks only ids 1..=12, refusing
/// the tag section (13) at `_ => return Err(WasmError::Decode("unsupported
/// section id"))`, line 4852. There is no instruction to lower a handler
/// onto, so the compiler encodes unwinding itself. Three designs were
/// weighed, and the deciding number is what each costs on the path that does
/// *not* throw, because that is every path in almost every program:
///
/// * **A sentinel value the caller tests.** An eighth tag, `thrown`. Rejected
///   on the measured growth law the value-representation experiment recorded:
///   one more type is one more test at every dispatch site, paid by every
///   program whether or not it contains a `throw`, and paid by `__typeof`,
///   `__truthy` and `__to_number` forever. It is also the disease this stack
///   refuses in its other spelling -- a completion record is not a language
///   value, and ECMA-262 does not make it one.
/// * **A table of handler continuations.** Needs a computed jump, which in
///   wasm means turning every function body into a `loop` around a
///   `br_table`. That is a rewrite of the non-throwing path to buy the
///   throwing one, which is the trade backwards.
/// * **A flag, and a check after every call that could throw.** Chosen.
///
/// So a throw in flight is three module globals -- [`Unwind`] -- and the
/// check is `global.get` + `br_if`: **two instructions and four bytes, at
/// each call to a user function and each `call_indirect`**. The `br_if`'s
/// target is the nearest enclosing handler, or, where there is none, the
/// function's own label, which returns -- and the pair already on the stack
/// is the callee's, so nothing has to be built to satisfy the return arity.
///
/// **A program with no `throw` in it emits none of this and pays nothing.**
/// Not one instruction, not one global: nothing else can set the flag, so
/// every check would be dead, and [`Scan::throws`] is what decides. That is
/// the number `a_program_that_cannot_throw_pays_nothing` in
/// `tests/conditional_and_try.rs` pins, against four module sizes measured
/// before the feature existed.
///
/// What that buys is the divergence worth naming: **a trap is not a throw.**
/// `try { undefined.a; } catch (e) {}` does not catch, because a property
/// access on a primitive is an `unreachable` and this engine has no `Error`
/// objects for it to be. Only a `throw` statement raises something a `catch`
/// can see.
pub(crate) mod m1 {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::array::{self, Ar};
    use crate::ast::m1 as ast;
    use crate::convert;
    use crate::diag::{Boundary, CompileError, host_table, unsupported};
    /// Research only -- Q1 variant C. Deleted when the track is decided.
    #[cfg(any(feature = "method-callsite", feature = "method-bound"))]
    use crate::method;
    use crate::ir::m1 as ir;
    use crate::opts::{HostFn, HostParam, HostResult, Names, Options};
    use crate::repr::{
        self, BlockType, Ins, ValType, WIDTH, box_array, box_bool, box_function, box_number,
        box_object, box_string, const_bool, const_null, const_number, const_string,
        const_undefined, drop_value, load_local, store_local, unbox_function, unbox_number,
        unbox_string,
    };
    /// Research only -- Q1 variant C. Deleted when the track is decided.
    #[cfg(feature = "method-callsite")]
    use crate::array::ARR_LEN;
    #[cfg(feature = "method-callsite")]
    use crate::repr::{is_array, is_string, unbox_array};
    use crate::runtime::{
        self, ALIGN_WORD, Conversions, Ctx, FN_ELEMENT, FN_ENV, FnBuild, Rt, STRING_HEADER,
        StringPool,
    };

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

    /// The module's funcref table, built as the lowering discovers which
    /// functions become values.
    ///
    /// Discovered rather than declared, because "is this function used as a
    /// value" is exactly the question the lowering answers by emitting
    /// [`const_function`] -- and asking it twice, once in a pre-pass and once
    /// here, is two chances to disagree. [`Scan`] predicts only the *yes or no*
    /// of it, and a debug assertion at the end of [`lower`] holds the two
    /// together.
    #[derive(Debug, Default)]
    struct FnTable {
        entries: Vec<ast::FuncId>,
        /// How many elements are already spoken for before the user's first.
        ///
        /// Two for a program that names `JSON`, whose `stringify` and `parse`
        /// adapters are in the same table and speak the same signature, and
        /// zero otherwise. They go **first** because their element indices are
        /// needed by the entry prologue, which is emitted while the script is
        /// being lowered -- that is, before the count of user adapters exists.
        /// Putting them last would need a number nobody has yet.
        reserved: i32,
    }

    impl FnTable {
        /// The element index of `id`, assigning one on its first use as a
        /// value. Element 0 is left null, so the index is one past the
        /// position -- which is what makes a zeroed payload uncallable even if
        /// something ever reached `call_indirect` without the tag test.
        fn element(&mut self, id: ast::FuncId) -> i32 {
            let position = self
                .entries
                .iter()
                .position(|entry| *entry == id)
                .unwrap_or_else(|| {
                    self.entries.push(id);
                    self.entries.len() - 1
                });
            position as i32 + 1 + self.reserved
        }
    }

    /// The one signature every call through a value speaks, and the arity it
    /// carries. `None` for a program with no function values and no indirect
    /// call sites, which is what keeps such a program's type section, table
    /// section and byte count exactly what they were.
    #[derive(Debug, Clone, Copy)]
    struct Uniform {
        type_index: u32,
        arity: u32,
    }

    /// Global 0 is the bump-allocation pointer, which is what
    /// [`Ctx::heap_global`] names; the script's bindings take two globals each
    /// after it.
    const HEAP_GLOBAL: u32 = 0;
    const BINDING_GLOBALS: u32 = HEAP_GLOBAL + 1;

    /// Where a throw in flight lives: a flag, and the thrown value beside it.
    ///
    /// Module globals rather than a return channel, because the value has to
    /// survive a `return` that the wasm type system already spent on the
    /// function's ordinary result. Three of them and not one: the thrown
    /// value is an ordinary V1 pair -- ECMA-262 lets any value be thrown, and
    /// this engine keeps that -- so it needs the same two words every other
    /// value needs, and the flag cannot be folded into the tag because
    /// `TAG_UNDEFINED` is 0 and `throw undefined` is a real program.
    ///
    /// Only present when [`Scan::throws`] is true, which is what keeps a
    /// program that cannot throw byte-identical to what it was.
    #[derive(Debug, Clone, Copy)]
    struct Unwind {
        /// `1` while a throw is looking for its handler, `0` otherwise.
        flag: u32,
        /// The thrown value's tag.
        tag: u32,
        /// The thrown value's payload.
        payload: u32,
    }

    impl Unwind {
        /// The three globals, taken in order from `base`.
        fn at(base: u32) -> Self {
            Self {
                flag: base,
                tag: base + 1,
                payload: base + 2,
            }
        }

        /// How many globals it occupies.
        const WORDS: u32 = 3;
    }

    /// Where the `JSON` namespace object lives, for a program that names it.
    ///
    /// One V1 pair of globals holding one object, built by `__json_ns` on the
    /// first top-level call and read by every occurrence of the name after
    /// that. Globals and not a fresh object per read for the reason ECMA-262
    /// 25.5 gives and the allocator seconds: `JSON === JSON` is `true`, and a
    /// bump heap that never frees must not allocate a namespace per mention.
    ///
    /// Built once per *instance* rather than once per call, guarded by the tag
    /// being `TAG_UNDEFINED`. A rebuild per call would be correct -- nothing
    /// holds a reference across the boundary -- and would leak two records and
    /// an object per invocation on an instance the embedder keeps, which is
    /// exactly the shape the downstream slot has.
    #[derive(Debug, Clone, Copy)]
    struct Json {
        tag: u32,
        payload: u32,
        /// Table elements of the two adapters, as `__json_ns` wants them.
        stringify_element: i32,
        parse_element: i32,
        /// Function index of `__json_ns`, which builds the object.
        ns: u32,
    }

    impl Json {
        /// How many globals it occupies.
        const WORDS: u32 = 2;
        /// `JSON.stringify` declares `(value, replacer, space)`, so the one
        /// uniform signature has to be at least this wide for its adapter to
        /// forward what its target reads.
        const ARITY: u32 = 3;
    }

    /// One enclosing `finally`, innermost last.
    ///
    /// A `finally` is the one construct here whose code runs on three
    /// different paths -- fall-through, `return`, and a throw -- and has to
    /// resume whichever one brought it. So the paths converge on one copy of
    /// the finalizer and `pending` says what to do afterwards; emitting the
    /// block once per path would triple it for every `return` in the try.
    #[derive(Debug, Clone, Copy)]
    struct Finalizer {
        /// Label depth of the block whose `end` the finalizer's code follows.
        /// Branching there is how a path enters it.
        depth: u32,
        /// The `i32` local holding what to resume: one of the three constants
        /// below.
        pending: u32,
        /// One value local, holding the pending `return`'s value *or* the
        /// pending throw's -- never both, because the two are alternatives.
        /// Parking the thrown value here rather than leaving it in
        /// [`Unwind`]'s globals is not tidiness: the finalizer may call a
        /// function that throws and catches internally, and that call would
        /// overwrite the globals with a value nobody is waiting for.
        slot: u32,
    }

    /// Nothing to resume: the try block simply finished.
    const PENDING_NONE: i32 = 0;
    /// Resume a `return` of [`Finalizer::slot`].
    const PENDING_RETURN: i32 = 1;
    /// Resume the throw of [`Finalizer::slot`].
    const PENDING_THROW: i32 = 2;

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
        // Imported functions take the first indices, so the runtime starts
        // exactly where the import table ends, and the conversions start
        // exactly where the runtime ends. Both bases are arithmetic on two
        // constant set lengths, which is what lets the runtime name a
        // conversion by index before either set is built.
        let runtime_base = table.imports();
        let convert_base = runtime_base + runtime::SET.len() as u32;
        // The JSON set sits between the conversions and the user's functions,
        // and is absent entirely for a program that never names `JSON`, which
        // is what leaves every such program's indices exactly where they were.
        let json_base = convert_base + convert::SET.len() as u32;
        let json_len = if scan.json {
            convert::JSON_SET.len() as u32
        } else {
            0
        };
        // The array set follows the JSON set, so a program with neither has
        // exactly the indices it always had, and a program with only `JSON`
        // keeps its own -- the two gates are independent and both append.
        let arr_base = json_base + json_len;
        let arr_len = if scan.arrays {
            array::SET.len() as u32
        } else {
            0
        };
        // Research only -- Q1 variant B: the whole set, whenever a program can
        // reach a String property at all. There is no per-method gating to do
        // here -- a bound method's body is reached through a table element,
        // and the element has to exist whether or not the source names it.
        // **That asymmetry is itself a result**: C can gate per method, B
        // cannot.
        #[cfg(feature = "method-bound")]
        let method_base = arr_base + arr_len;
        #[cfg(feature = "method-bound")]
        let method_len = if scan.bound {
            method::Plan::bound_surface().len()
        } else {
            0
        };
        #[cfg(feature = "method-bound")]
        let user_base = method_base + method_len;
        // Research only -- Q1 variant C, appended last for the same reason
        // every other gated set is: a program without it keeps the indices it
        // always had. Deleted when the track is decided.
        #[cfg(feature = "method-callsite")]
        let method_base = arr_base + arr_len;
        #[cfg(feature = "method-callsite")]
        let method_len = scan.methods.len();
        #[cfg(feature = "method-callsite")]
        let user_base = method_base + method_len;
        #[cfg(not(any(feature = "method-callsite", feature = "method-bound")))]
        let user_base = arr_base + arr_len;
        let ctx = Ctx {
            func_base: runtime_base,
            heap_global: HEAP_GLOBAL,
            type_names: scan.type_of.then(|| runtime::TypeNames::intern(&mut pool)),
            string_length: (scan.string_length
                || (cfg!(feature = "method-bound") && bound_wanted(&scan)))
            .then(|| pool.intern("length")),
            prim_names: runtime::PrimNames::intern(&mut pool),
            conversions: Conversions {
                num_to_string: convert_base + convert::Cv::NumToString.offset(),
                str_to_num: convert_base + convert::Cv::StrToNum.offset(),
                str_cmp: convert_base + convert::Cv::StrCmp.offset(),
            },
            arrays: scan.arrays,
            #[cfg(feature = "method-bound")]
            bound_trim: bound_wanted(&scan).then(|| {
                (
                    method_base,
                    // Element 1: the first reserved slot after the null one.
                    // `JSON` takes two when present, so this follows them.
                    1 + if scan.json { 2 } else { 0 },
                    pool.intern("trim"),
                )
            }),
            captures: scan.captures,
        };
        let cv = convert::Ctx {
            func_base: convert_base,
            runtime_base,
            names: convert::Names::intern(&mut pool),
        };

        // The three unwind globals sit *after* every binding global, so a
        // program that cannot throw has the same global indices it always
        // had. Computed here rather than where the globals are built, because
        // the JSON set is handed the indices and is built before them.
        let binding_globals = BINDING_GLOBALS + program.script().bindings.len() as u32 * WIDTH;
        let unwind = scan.throws.then(|| Unwind::at(binding_globals));

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

        // The JSON set is built only for a program that names `JSON`, and it
        // is handed the unwind globals so that its refusals are catchable
        // rather than traps -- `scan` guarantees they exist, because naming
        // `JSON` is what sets `throws`.
        let json_set: Vec<runtime::RtFunc> = if scan.json {
            let jcx = convert::JsonCtx {
                func_base: json_base,
                runtime_base,
                convert_base,
                unwind: unwind.map(|u| convert::Throwing {
                    flag: u.flag,
                    tag: u.tag,
                    payload: u.payload,
                }),
                arrays: arr_base,
                names: convert::JsonNames::intern(&mut pool),
            };
            convert::build_json(&jcx)
        } else {
            Vec::new()
        };

        let array_set: Vec<runtime::RtFunc> = if scan.arrays {
            array::build(&array::Ctx {
                func_base: arr_base,
                runtime_base,
                names: array::Names::intern(&mut pool),
            })
        } else {
            Vec::new()
        };

        #[cfg(feature = "method-callsite")]
        let method_set: Vec<runtime::RtFunc> = if scan.methods.is_empty() {
            Vec::new()
        } else {
            method::build(&method::Ctx {
                func_base: method_base,
                runtime_base,
                plan: scan.methods.clone(),
                array_base: arr_base,
            })
        };
        #[cfg(feature = "method-bound")]
        let method_set: Vec<runtime::RtFunc> = if scan.bound {
            method::build(&method::Ctx {
                func_base: method_base,
                runtime_base,
                plan: method::Plan::bound_surface(),
                array_base: arr_base,
            })
        } else {
            Vec::new()
        };
        #[cfg(not(any(feature = "method-callsite", feature = "method-bound")))]
        let method_set: Vec<runtime::RtFunc> = Vec::new();

        let mut funcs: Vec<ir::Func> = Vec::new();
        for built in runtime::build(&ctx)
            .into_iter()
            .chain(convert::build(&cv))
            .chain(json_set)
            .chain(array_set)
            .chain(method_set)
        {
            let type_index = intern(&mut types, built.params.clone(), built.results.clone());
            funcs.push(func(
                built.name.to_string(),
                type_index,
                built.locals,
                built.body,
            ));
        }
        debug_assert_eq!(
            funcs.len() as u32,
            user_base - runtime_base,
            "the set lengths are what every index above was computed from"
        );

        // The uniform signature has to exist before the first call site names
        // it, and only for a program that has one: an unused type-section entry
        // would make every script pay a byte for a capability it never used.
        // The uniform signature leads with an `i32` environment once anything
        // in the program captures. It is one signature for the whole table, so
        // the widening is all-or-nothing -- an adapter whose target captures
        // nothing simply never reads slot 0. Gated, so a closure-free program
        // interns the type it always did.
        let uniform = scan.needs_table().then(|| {
            let mut params = values(scan.uniform_arity(program));
            if scan.captures {
                params.insert(0, ValType::I32);
            }
            Uniform {
                type_index: intern(&mut types, params, values(1)),
                arity: scan.uniform_arity(program),
            }
        });

        // `JSON`'s two globals follow the unwind channel's three, so nothing
        // that existed before either of them moves. Its two table elements
        // follow every adapter a user function took, for the same reason.
        let json = scan.json.then(|| Json {
            tag: binding_globals + Unwind::WORDS,
            payload: binding_globals + Unwind::WORDS + 1,
            stringify_element: 1,
            parse_element: 2,
            ns: json_base + convert::Js::Ns.offset(),
        });

        let mut fns = FnTable {
            reserved: if scan.json { 2 } else { 0 }
                // Research only -- Q1 variant B: one element for `__m_trim`'s
                // adapter. **Reserved whenever a String property can be read
                // at all**, named or not, because the element index is baked
                // into `obj_get` -- which is emitted before anyone knows
                // whether a `trim` appears. C pays nothing for a method it
                // does not call; B pays for the element regardless.
                + if cfg!(feature = "method-bound") && bound_wanted(&scan) {
                    1
                } else {
                    0
                },
            ..FnTable::default()
        };
        for (index, function) in program.functions.iter().enumerate() {
            let id = ast::FuncId(index as u32);
            let built = Lower::new(
                program,
                &ctx,
                &scan.value_bindings,
                &mut pool,
                &table,
                &mut fns,
                uniform,
                scan.arrays.then_some(arr_base),
                #[cfg(feature = "method-callsite")]
                (!scan.methods.is_empty()).then(|| (method_base, scan.methods.clone())),
                unwind,
                json,
                user_base,
                scan.captures,
                id,
            )
            .function()?;
            let arity = if id == ast::Program::SCRIPT {
                program.arg_count
            } else {
                function.params.len() as u32
            };
            // A capturing function's signature leads with its environment
            // pointer. Every other function's is exactly what it always was,
            // which is the whole of `plan/design-closure-milestone.md` §1.2 at
            // the signature level.
            let mut params = values(arity);
            if !function.captures.is_empty() {
                params.insert(0, ValType::I32);
            }
            let type_index = intern(&mut types, params, values(1));
            funcs.push(func(
                debug_name(program, id),
                type_index,
                built.local_groups(),
                built.body,
            ));
        }

        debug_assert_eq!(
            scan.function_values,
            !fns.entries.is_empty(),
            "the scan and the lowering disagree about whether this program makes a function a value"
        );

        // One adapter per table entry, appended after every user function so
        // no existing index moves. Its body is the whole of the arity
        // reconciliation: forward what the target declares, and let the rest of
        // the uniform parameter list fall away unread.
        let adapter_base = user_base + program.functions.len() as u32;
        let mut elements = Vec::new();
        let mut table_type = None;
        if !fns.entries.is_empty()
            || scan.indirect
            || scan.json
            || (cfg!(feature = "method-bound") && bound_wanted(&scan))
        {
            let uniform = uniform.expect("a table means the uniform signature exists");
            let mut in_table = Vec::new();
            // `JSON`'s two, first, because `FnTable::reserved` promised the
            // elements to them. Each forwards what its target declares --
            // three JS values for `stringify`, two for `parse` -- exactly as a
            // user function's adapter does; there is no shape here that a
            // script's own function value does not also have.
            // Research only -- Q1 variant B. After `JSON`'s two, matching the
            // element index `obj_get` was handed.
            #[cfg(feature = "method-bound")]
            if scan.bound {
                let env_slot = u32::from(scan.captures);
                let _ = env_slot;
                let mut body: Vec<Ins> = Vec::new();
                // The receiver, out of the two-word environment `__m_bind`
                // built. Slot 0 is the environment pointer.
                body.push(Ins::LocalGet(0));
                body.push(Ins::I32Load(ALIGN_WORD, 0));
                body.push(Ins::LocalGet(0));
                body.push(Ins::I64Load(ALIGN_WORD, 4));
                body.push(Ins::Call(method_base + method::Me::Trim.offset_in(&ctx)));
                let index = adapter_base + in_table.len() as u32;
                funcs.push(func(
                    "<adapter of __m_trim>".to_string(),
                    uniform.type_index,
                    Vec::new(),
                    body,
                ));
                in_table.push(index);
            }
            if json.is_some() {
                for (offset, arity) in [
                    (convert::Js::Stringify.offset(), Json::ARITY),
                    (convert::Js::Parse.offset(), 2),
                ] {
                    // `JSON`'s two capture nothing, so they skip slot 0 the
                    // same way a non-capturing user function's adapter does.
                    let env_slot = u32::from(scan.captures);
                    let mut body: Vec<Ins> = (0..arity * WIDTH)
                        .map(|i| Ins::LocalGet(env_slot + i))
                        .collect();
                    body.push(Ins::Call(json_base + offset));
                    let index = adapter_base + in_table.len() as u32;
                    funcs.push(func(
                        format!(
                            "<adapter of {}>",
                            convert::JSON_SET[offset as usize].symbol()
                        ),
                        uniform.type_index,
                        Vec::new(),
                        body,
                    ));
                    in_table.push(index);
                }
            }
            for (position, id) in fns.entries.iter().enumerate() {
                let arity = program.func(*id).params.len() as u32;
                debug_assert!(
                    arity <= uniform.arity,
                    "the uniform arity bounds every parameter list"
                );
                // Slot 0 is the environment when the program has closures. An
                // adapter whose target captures **forwards** it; one whose
                // target does not simply leaves it unread, exactly as it
                // already leaves surplus arguments unread (13.3.8.1).
                let env_slot = u32::from(scan.captures);
                let mut body: Vec<Ins> = Vec::new();
                if !program.func(*id).captures.is_empty() {
                    body.push(Ins::LocalGet(0));
                }
                body.extend((0..arity * WIDTH).map(|i| Ins::LocalGet(env_slot + i)));
                body.push(Ins::Call(user_base + id.0));
                funcs.push(func(
                    format!("<adapter of {}>", debug_name(program, *id)),
                    uniform.type_index,
                    Vec::new(),
                    body,
                ));
                in_table.push(adapter_base + fns.reserved as u32 + position as u32);
            }
            if !in_table.is_empty() {
                elements.push(ir::Elem {
                    offset: 1,
                    funcs: in_table,
                });
            }
            // One past the last element, because element 0 is the null one.
            table_type = Some(ir::Table {
                min: fns.entries.len() as u32 + fns.reserved as u32 + 1,
                max: None,
            });
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
        if let Some(unwind) = unwind {
            debug_assert_eq!(
                unwind.flag,
                globals.len() as u32,
                "the unwind globals follow every binding global"
            );
            // Flag, tag, payload -- and the flag starts clear, so a module
            // that is instantiated and never called has no throw in flight.
            globals.push(ir::Global {
                ty: ir::ValType::I32,
                mutable: true,
                init: ir::Const::I32(0),
            });
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
            debug_assert_eq!(
                globals.len() as u32,
                unwind.flag + Unwind::WORDS,
                "the three words are the whole of an unwind channel"
            );
        }
        if let Some(json) = json {
            debug_assert_eq!(
                json.tag,
                globals.len() as u32,
                "`JSON`'s pair follows the unwind channel"
            );
            // `(0, 0)` is `undefined`, and the entry prologue reads exactly
            // that to decide whether the namespace has been built yet.
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
            debug_assert_eq!(globals.len() as u32, json.tag + Json::WORDS);
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
            table: table_type,
            memory: Some(ir::Memory {
                min: memory_pages(&pool)?,
                max: None,
            }),
            globals,
            funcs,
            elements,
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
                Ins::CallIndirect(ty, table) => ir::Ins::CallIndirect(ty, table),
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
        /// Whether the program ever puts a function where a value goes. The
        /// yes-or-no of what [`FnTable`] then counts exactly, and the reason
        /// a program that never does emits no table and no uniform signature.
        function_values: bool,
        /// The function bindings whose *name* is read as a value somewhere --
        /// `f` rather than `f()`.
        ///
        /// Those are the only bindings that need storage and a function
        /// object built into it. A `function f(){}` that is only ever called
        /// keeps costing nothing: no global, no allocation, no table element.
        /// The object it would hold has no way to be observed, and ECMA-262
        /// makes it exist rather than makes it visible.
        value_bindings: BTreeSet<ast::BindingId>,
        /// Whether the program calls anything that is not a statically known
        /// function. Tracked separately from `function_values` because a
        /// module can need the table for one without the other -- a script
        /// that only ever calls `o.m()` on an object a host built has indirect
        /// calls and no values of its own -- and `call_indirect` needs table 0
        /// to exist either way.
        indirect: bool,
        /// The widest argument list at an indirect call site.
        call_arity: u32,
        /// Whether any function in the program captures a binding of an
        /// enclosing one.
        ///
        /// The closure gate. It widens the uniform call signature by one
        /// leading `i32` and the function record by one word, and it does
        /// neither for a program that has no closure -- which is the promise
        /// `plan/design-closure-milestone.md` §1.2 makes and §2.4 asks to be
        /// gated. Exact by construction: it is a property of the resolved
        /// tree, read straight off `Function::captures`, not a guess about
        /// syntax.
        captures: bool,
        /// Research only -- Q1 variant B. Whether the program reads one of
        /// B's bound-method names off anything. Separate from
        /// [`string_length`](Self::string_length) on purpose: sharing that
        /// flag made `return "ab".length;` pay 514 bytes for a bound surface
        /// it never touches, which fails the experiment's own §1.2.
        #[cfg(feature = "method-bound")]
        bound: bool,
        /// Research only -- Q1 variant C. Whether the program calls one of the
        /// four methods by name. Deleted when the track is decided.
        #[cfg(feature = "method-callsite")]
        methods: method::Plan,
        /// Whether the program can read a property off a String, which is:
        /// it writes `.length` as a static key, or it writes any computed key
        /// at all. Gates the one arm of [`runtime::obj_get`] that answers
        /// `"ab".length` and the four bytes that arm compares against.
        ///
        /// Not exact in the computed case, and it cannot be: whether a
        /// receiver is a String is a run-time fact. Exact in the static case,
        /// which is the one that matters -- a program with no `.length` and no
        /// `o[k]` in it is byte-identical to what it was.
        string_length: bool,
        /// Whether the program can produce an Array, which is exactly: it
        /// writes an ArrayLiteral, or it names `JSON`.
        ///
        /// `JSON` is in the predicate because `JSON.parse` builds an array out
        /// of text the compiler never sees. Nothing else can bring one into
        /// existence -- a computed access in a program with neither can never
        /// find an array to index -- so the predicate is *exact*, which is the
        /// property `json`'s own gate is chosen for and the property a gate
        /// has to have to be worth having. Set in one place,
        /// [`Scan::finish_arrays`], so the two halves cannot drift.
        arrays: bool,
        /// Whether the program names this engine's `JSON` -- see
        /// [`ast::Res::Json`].
        ///
        /// The predicate is exact and not an over-approximation, which is what
        /// earns the gate: a program that never writes the name emits none of
        /// [`convert::JSON_SET`], no adapter, no element and no global, and is
        /// byte-identical to what it was.
        json: bool,
        /// Whether the program writes a `throw` anywhere, **or** names `JSON`.
        ///
        /// The yes-or-no that decides whether this module carries any
        /// unwinding machinery at all. Nothing else can set the in-flight
        /// flag -- a trap is not a throw -- so in a program with no `throw`
        /// every check would be dead and every global unread, and none of
        /// them is emitted. See this module's header.
        ///
        /// `JSON` is the second producer, and it is the reason this is not
        /// simply "the program writes `throw`": `JSON.parse` raises one for a
        /// text that is not JSON, and `fleet.js` catches exactly that with a
        /// `try`/`catch` that has no `throw` statement anywhere near it. The
        /// condition is stated at `convert::JsonCtx::unwind` and satisfied in
        /// [`scan`].
        throws: bool,
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

        /// How many JavaScript arguments the one uniform signature carries.
        ///
        /// A bound rather than a measurement, and deliberately a coarse one:
        /// the widest parameter list in the program -- not the widest among the
        /// functions that turn out to be values -- or the widest indirect call
        /// site, whichever is more. The exact answer is only knowable after
        /// lowering, which is after the call sites that name this type have
        /// already been emitted. See this module's header for what the
        /// looseness costs.
        ///
        /// The script's own `$N` are not parameters of a *function* the table
        /// can hold, so they do not enter: `Function::params` is empty for the
        /// script.
        fn uniform_arity(&self, program: &ast::Program) -> u32 {
            program
                .functions
                .iter()
                .map(|f| f.params.len() as u32)
                .max()
                .unwrap_or(0)
                .max(self.call_arity)
                // `JSON.stringify`'s adapter is in the same table and speaks
                // the same signature, so its three declared parameters are a
                // floor for a program that names `JSON`. `fleet.js` already
                // reaches three (`ui.input.pointer`), so it pays nothing.
                .max(if self.json { Json::ARITY } else { 0 })
        }

        /// Whether this program needs the module's funcref table at all.
        fn needs_table(&self) -> bool {
            // Research only -- Q1 variant B: a bound method is a function
            // value, so a program that reads one needs the table and the
            // uniform signature even if it holds no function of its own.
            // Another fixed cost C does not pay.
            #[cfg(feature = "method-bound")]
            let bound = self.bound;
            #[cfg(not(feature = "method-bound"))]
            let bound = false;
            self.function_values || self.indirect || self.json || bound
        }

        /// One call that has to go through the table.
        fn note_indirect(&mut self, args: u32) {
            self.indirect = true;
            self.call_arity = self.call_arity.max(args);
        }
    }

    /// Research only -- Q1 variant B. Whether this program reaches a bound
    /// method at all.
    fn bound_wanted(_scan: &Scan) -> bool {
        #[cfg(feature = "method-bound")]
        {
            _scan.bound
        }
        #[cfg(not(feature = "method-bound"))]
        {
            false
        }
    }

    fn scan(program: &ast::Program) -> Result<Scan, CompileError> {
        let mut out = Scan::default();
        for function in &program.functions {
            for stmt in &function.body {
                host_stmt(program, stmt, &mut out)?;
            }
        }
        // The condition `convert::JsonCtx::unwind` states in its own words:
        // *a program that mentions `JSON` needs the channel whether or not it
        // writes `throw`*. Without this a `JSON.parse` refusal would trap
        // where the script had written a `catch` for it -- which is `fleet.js`
        // lines 15 to 19, the reason the feature exists.
        out.throws |= out.json;
        // The other half of the array gate. `JSON.parse` can return an array
        // from text no ArrayLiteral appears in, so a program that names `JSON`
        // needs the set whether or not it writes `[`. Both halves are set
        // here so neither can be forgotten at the other's site.
        out.arrays |= out.json;
        // Read off the resolved tree rather than accumulated while walking:
        // `record_captures` has already settled every function's layout by
        // the time a `Program` exists, so the scan only has to ask.
        out.captures = program.functions.iter().any(|f| !f.captures.is_empty());
        // Research only -- Q1 variant B. A bound method's function record has
        // to carry an environment, and the record only *has* an environment
        // slot when the program has closures. So B **turns the closure
        // machinery on** for a program that has no closure in it -- a fixed
        // cost that variant C does not pay, and one of B's leaks.
        #[cfg(feature = "method-bound")]
        {
            out.captures |= out.bound;
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

    fn host_stmt(
        program: &ast::Program,
        stmt: &ast::Stmt,
        scan: &mut Scan,
    ) -> Result<(), CompileError> {
        match &stmt.kind {
            ast::StmtKind::Empty | ast::StmtKind::Func { .. } => Ok(()),
            ast::StmtKind::Expr(e) => host_expr(e, scan),
            ast::StmtKind::Decl(declarators) => declarators.iter().try_for_each(|d| {
                match (&d.init, program.binding(d.binding).kind) {
                    // `const f = function () {}`: the declarator builds a
                    // function object only when the name is read as a value
                    // somewhere, and whether it is cannot be known until the
                    // whole program has been walked. So nothing is decided
                    // here; `Lower::declarator` asks `value_bindings`, which
                    // this walk is what fills in.
                    (
                        Some(ast::Expr {
                            kind: ast::ExprKind::Function(_),
                            ..
                        }),
                        ast::BindingKind::Function(_),
                    ) => Ok(()),
                    (Some(init), _) => host_expr(init, scan),
                    (None, _) => Ok(()),
                }
            }),
            ast::StmtKind::Block(stmts) => {
                stmts.iter().try_for_each(|s| host_stmt(program, s, scan))
            }
            ast::StmtKind::If { test, then, alt } => {
                host_expr(test, scan)?;
                host_stmt(program, then, scan)?;
                alt.iter().try_for_each(|s| host_stmt(program, s, scan))
            }
            ast::StmtKind::While { test, body } => {
                host_expr(test, scan)?;
                host_stmt(program, body, scan)
            }
            ast::StmtKind::For {
                init,
                test,
                update,
                body,
            } => {
                init.iter().try_for_each(|s| host_stmt(program, s, scan))?;
                test.iter().try_for_each(|e| host_expr(e, scan))?;
                update.iter().try_for_each(|e| host_expr(e, scan))?;
                host_stmt(program, body, scan)
            }
            ast::StmtKind::Return(value) => value.iter().try_for_each(|e| host_expr(e, scan)),
            ast::StmtKind::Throw(value) => {
                scan.throws = true;
                host_expr(value, scan)
            }
            ast::StmtKind::Try {
                block,
                handler,
                finalizer,
            } => {
                block.iter().try_for_each(|s| host_stmt(program, s, scan))?;
                if let Some(catch) = handler {
                    catch
                        .body
                        .iter()
                        .try_for_each(|s| host_stmt(program, s, scan))?;
                }
                match finalizer {
                    Some(body) => body.iter().try_for_each(|s| host_stmt(program, s, scan)),
                    None => Ok(()),
                }
            }
        }
    }

    /// Walk one expression in the same shape [`Lower::expr`] walks it.
    ///
    /// The two must agree about exactly one thing -- where a function reaches a
    /// value position -- and the three places they could disagree are the
    /// three exceptions written out below: a host call's callee, a direct
    /// call's callee, and an immediately-invoked function expression. The
    /// fourth, `const f = function () {}`, is in [`host_stmt`], which is why
    /// that one takes the program and this one does not. A debug assertion in
    /// [`lower`] checks the agreement rather than trusting it.
    fn host_expr(expr: &ast::Expr, scan: &mut Scan) -> Result<(), CompileError> {
        match &expr.kind {
            ast::ExprKind::Int(_)
            | ast::ExprKind::Num(_)
            | ast::ExprKind::Str(_)
            | ast::ExprKind::Bool(_)
            | ast::ExprKind::Null
            | ast::ExprKind::Undefined
            | ast::ExprKind::Arg(_) => Ok(()),
            // Reached here rather than as a callee: this one is a value.
            ast::ExprKind::Function(_) => {
                scan.function_values = true;
                Ok(())
            }
            // A bare host name is a zero-argument call, as it is at M0.
            ast::ExprKind::Name(name) => match &name.res {
                ast::Res::Host(text) => note_host(scan, text, 0, expr.span),
                // A name bound to a known function, read and not called. The
                // read is the constant function value.
                ast::Res::Callee(id) => {
                    scan.function_values = true;
                    scan.value_bindings.insert(*id);
                    Ok(())
                }
                ast::Res::Json => {
                    scan.json = true;
                    Ok(())
                }
                _ => Ok(()),
            },
            ast::ExprKind::Call { callee, args } => {
                // Research only -- Q1 variant C. The gate: the program calls
                // one of the four by name. Not exact -- `o.trim()` on a plain
                // object turns the set on and never reaches the prefab -- and
                // it cannot be, for the reason `specialised_method` gives.
                #[cfg(feature = "method-callsite")]
                if let ast::ExprKind::Member {
                    key: ast::MemberKey::Static(name),
                    ..
                } = &callee.kind
                    && let Some(me) = method::Me::at_call_site(name, args.len())
                {
                    scan.methods.want(me);
                    // A cross-gate dependency, and a leak worth naming where
                    // it happens: `__m_push` appends with `__arr_push`, which
                    // lives behind the *array* gate. So a `push` call site has
                    // to reach across and turn a second, unrelated gate on.
                    if me.needs_arrays() {
                        scan.arrays = true;
                    }
                }
                // The callee of a host call, of a direct call, and of an
                // immediately-invoked function expression is the *call* and
                // not a value, so none of the three is walked as one. Every
                // other callee is an ordinary value expression, and the call
                // through it needs the table.
                match &callee.kind {
                    ast::ExprKind::Name(name) => match &name.res {
                        ast::Res::Host(text) => {
                            note_host(scan, text, args.len() as u32, expr.span)?;
                        }
                        ast::Res::Callee(_) => {}
                        _ => {
                            scan.note_indirect(args.len() as u32);
                            host_expr(callee, scan)?;
                        }
                    },
                    ast::ExprKind::Function(_) => {}
                    _ => {
                        scan.note_indirect(args.len() as u32);
                        host_expr(callee, scan)?;
                    }
                }
                args.iter().try_for_each(|a| host_expr(a, scan))
            }
            ast::ExprKind::Object(properties) => properties
                .iter()
                .try_for_each(|property| host_expr(&property.value, scan)),
            ast::ExprKind::Array(elements) => {
                scan.arrays = true;
                elements.iter().try_for_each(|el| host_expr(el, scan))
            }
            ast::ExprKind::Member { object, key } => {
                host_expr(object, scan)?;
                match key {
                    ast::MemberKey::Static(name) => {
                        scan.string_length |= name == "length";
                        // Research only -- Q1 variant B. Its bound methods are
                        // answered inside `obj_get`'s String branch, which is
                        // gated by this same flag, so reading one has to turn
                        // the branch on. **A coarser gate than C's**: C keys
                        // on the *call site*, B on any String property read
                        // that could be one of its names.
                        #[cfg(feature = "method-bound")]
                        {
                            scan.bound |= name == "trim";
                        }
                        Ok(())
                    }
                    ast::MemberKey::Computed(key) => {
                        scan.computed_key = true;
                        // A computed key *could* be the string "length", so
                        // the flag goes on -- unless the text already settles
                        // it. A number literal is never that string, and a
                        // string literal is or is not, in the source. This is
                        // what keeps `a[0]` and `o["a"]` from turning the arm
                        // on for every program that indexes anything.
                        scan.string_length |= match &key.kind {
                            ast::ExprKind::Int(_) | ast::ExprKind::Num(_) => false,
                            ast::ExprKind::Str(text) => text == "length",
                            _ => true,
                        };
                        host_expr(key, scan)
                    }
                }
            }
            ast::ExprKind::Conditional { test, then, alt } => {
                host_expr(test, scan)?;
                host_expr(then, scan)?;
                host_expr(alt, scan)
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
        /// A **captured** binding this function owns: the raw local at this
        /// index holds an `i32` pointer to the binding's heap cell.
        ///
        /// It reuses the `i32` half of the pair of locals the binding would
        /// otherwise occupy, so the local layout is unchanged and `slot`
        /// still indexes it. The `i64` half goes unread, which costs a
        /// declared local and no instructions -- against renumbering every
        /// binding after a captured one, which would cost a second slot
        /// mapping that has to agree with the first.
        Cell(u32),
        /// A binding captured **from an enclosing function**: this function's
        /// environment holds the cell pointer at this index.
        Env(u32),
    }

    /// One captured binding's storage: `[tag: i32][payload: i64]`, the V1 pair
    /// stored whole exactly as an object entry or an array element stores one.
    ///
    /// Twelve bytes and not eight: a cell holds a *JavaScript value*, and
    /// narrowing it to a payload would mean the cell deciding a type the
    /// binding never had.
    const CELL_BYTES: i32 = 12;
    const CELL_TAG: u32 = 0;
    const CELL_PAYLOAD: u32 = 4;

    /// An environment is `[cell: i32]*` and nothing else.
    ///
    /// No length word: every index into it is a compile-time constant --
    /// `Function::captures`'s position -- so a length would be a word written
    /// once and read never. `plan/design-closure-milestone.md` §2.5 specified
    /// `[n][cells…]`; the note is corrected rather than the word emitted.
    const ENV_SLOT: i32 = 4;

    /// Which pair of accessors a member expression reaches. See
    /// [`Lower::accessor`].
    #[derive(Debug, Clone, Copy)]
    enum Accessor {
        /// `__obj_get` / `__obj_set`, taking the key as an interned string
        /// record. Every Static key, and every key at all in a program with no
        /// arrays.
        Obj,
        /// `__prop_get` / `__prop_set`, taking the key as a whole V1 pair so
        /// an index never becomes digits. Carries the index of `__arr_new`.
        Prop(u32),
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
            /// Where the key is held between evaluating it and writing.
            key: TargetKey,
        },
    }

    /// How a held key is stored, which is [`Accessor`]'s distinction seen from
    /// the assignment side: the accessor a target will reach decides the shape
    /// its key has to be kept in, and holding the wrong one would mean
    /// converting it back at the write.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TargetKey {
        /// One raw `i32`: the interned string record, already ToPropertyKey'd.
        /// What `__obj_get`/`__obj_set` take.
        Raw(u32),
        /// One value local: the whole V1 pair, so `__prop_set` can still see a
        /// Number where the source wrote one. Carries the index of
        /// `__arr_new` beside it, because the write needs both.
        Pair { slot: u32, base: u32 },
    }

    struct Lower<'a> {
        program: &'a ast::Program,
        ctx: &'a Ctx,
        /// Which function bindings hold an object -- see [`Scan`].
        value_bindings: &'a BTreeSet<ast::BindingId>,
        pool: &'a mut StringPool,
        table: &'a Table,
        /// The funcref table, which every function value in the program shares
        /// and which grows as they are found.
        fns: &'a mut FnTable,
        /// The signature every call through a value speaks, or `None` for a
        /// program the scan said has no such call and no such value.
        uniform: Option<Uniform>,
        /// Whether *any* function in the program captures: the gate that
        /// decides the record's width and the uniform signature's shape. Not
        /// about this function -- `env_param` is that one.
        captures: bool,
        /// `1` when this function takes a leading environment parameter --
        /// that is, when it captures anything -- and `0` otherwise.
        ///
        /// Every binding local is offset by it, so a function that captures
        /// nothing has exactly the local layout it had before closures
        /// existed. Wasm local `0` of a capturing function is its environment.
        env_param: u32,
        /// Index of `__arr_new`, or `None` for a program the array gate
        /// refused -- which emits none of that set and keeps the pre-array
        /// lowering of a computed access, exactly as it was.
        arrays: Option<u32>,
        /// Research only -- Q1 variant C. Index of `__m_ws_width`, or `None`
        /// for a program that calls none of the four methods.
        #[cfg(feature = "method-callsite")]
        methods: Option<(u32, method::Plan)>,
        /// Where a throw in flight lives, or `None` for a program with no
        /// `throw` in it -- which emits no check and no global.
        unwind: Option<Unwind>,
        /// Where the `JSON` namespace object lives, or `None` for a program
        /// that never names it -- which emits nothing of it at all.
        json: Option<Json>,
        user_base: u32,
        id: ast::FuncId,
        f: FnBuild,
        /// How many wasm labels are open around the cursor, not counting the
        /// function's own. A branch to a target this frame did not just open
        /// is `self.depth - target`, and the function's own label -- which is
        /// a `return` -- is `self.depth` exactly.
        ///
        /// Maintained by [`Lower::push`] and by nothing else, which is why
        /// every structured instruction this module emits goes through it. A
        /// `repr` or `runtime` helper that writes straight into `f.body`
        /// cannot break the count: those runs are balanced and none of them
        /// branches out.
        depth: u32,
        /// The enclosing `catch`/`finally` entries a throw may reach, as
        /// label depths, innermost last. Empty means a throw leaves this
        /// function.
        handlers: Vec<u32>,
        /// The enclosing `finally` blocks a `return` has to run on its way
        /// out, innermost last.
        finalizers: Vec<Finalizer>,
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
        #[allow(clippy::too_many_arguments)]
        fn new(
            program: &'a ast::Program,
            ctx: &'a Ctx,
            value_bindings: &'a BTreeSet<ast::BindingId>,
            pool: &'a mut StringPool,
            table: &'a Table,
            fns: &'a mut FnTable,
            uniform: Option<Uniform>,
            arrays: Option<u32>,
            #[cfg(feature = "method-callsite")] methods: Option<(u32, method::Plan)>,
            unwind: Option<Unwind>,
            json: Option<Json>,
            user_base: u32,
            captures: bool,
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
            // A capturing function's environment is its first wasm parameter.
            // Zero for every other function, which is why a program with no
            // closure emits the parameter lists it always did.
            let env_param = u32::from(!function.captures.is_empty());
            let mut f = FnBuild::new(env_param + arity * WIDTH);
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
                value_bindings,
                pool,
                table,
                fns,
                uniform,
                arrays,
                #[cfg(feature = "method-callsite")]
                methods,
                unwind,
                json,
                user_base,
                captures,
                id,
                env_param,
                f,
                depth: 0,
                handlers: Vec::new(),
                finalizers: Vec::new(),
                completion,
                free: Vec::new(),
                free_raw: Vec::new(),
            }
        }

        /// One statement list: the function declarations directly in it are
        /// instantiated first, then the statements run.
        ///
        /// That order is ECMA-262 10.2.11 FunctionDeclarationInstantiation for
        /// a function body and 14.2.3 BlockDeclarationInstantiation for a
        /// block, and it is what makes `f()` above `function f(){}` work.
        /// Running it *per entry to the list* is the other half: a declaration
        /// in a loop body is a new function object on every pass, which is the
        /// same rule as `let`.
        ///
        /// The two places a `StmtKind::Func` can appear are a function body
        /// and a block, and both come through here. It cannot be the bare body
        /// of an `if`, a `while` or a `for`: the parser refuses a declaration
        /// there with a diagnostic of its own, which
        /// `a_function_declaration_is_not_a_statement_body` in `parse_m1.rs`
        /// holds.
        fn stmts(&mut self, stmts: &[ast::Stmt]) -> Result<(), CompileError> {
            for stmt in stmts {
                if let ast::StmtKind::Func { binding, func } = &stmt.kind {
                    self.instantiate(*binding, *func);
                }
            }
            stmts.iter().try_for_each(|s| self.stmt(s))
        }

        /// Build the function object a `BindingKind::Function` binding holds,
        /// and store it.
        ///
        /// Nothing at all when no occurrence of the name is a *value*: the
        /// object would then be unobservable, and the whole cost of it -- a
        /// pair of globals or locals, a table element, an adapter and an
        /// allocation -- is what a script that only declares and calls used to
        /// not pay and still does not.
        fn instantiate(&mut self, binding: ast::BindingId, func: ast::FuncId) {
            if !self.value_bindings.contains(&binding) {
                return;
            }
            let place = self.place(binding);
            self.function_value(func);
            self.store(place);
        }

        /// One fresh function object, on the stack: 15.2.5's "a new object"
        /// for a FunctionExpression and 10.2.11's for a declaration.
        /// One function object: its table element, and -- once any function
        /// in the program captures -- its environment.
        ///
        /// A function that captures nothing still gets the word, holding 0.
        /// The record has to be one shape for `indirect_call` to read `FN_ENV`
        /// without knowing which function it holds, and the gate is what keeps
        /// a closure-free program from carrying the word at all.
        fn function_value(&mut self, func: ast::FuncId) {
            let element = self.fns.element(func);
            let new = self.ctx.call(Rt::FnNew);
            if self.captures {
                let mut inner = vec![Ins::I32Const(element)];
                if self.program.func(func).captures.is_empty() {
                    inner.push(Ins::I32Const(0));
                    inner.push(new);
                    box_function(&inner, &mut self.f.body);
                } else {
                    // `build_env` emits into the body, so the element const has
                    // to be there first for the argument order to hold.
                    self.push(Ins::I32Const(element));
                    self.build_env(func);
                    self.push(new);
                    let value = self.take_raw();
                    self.push(Ins::LocalSet(value));
                    box_function(&[Ins::LocalGet(value)], &mut self.f.body);
                    self.give_raw(value);
                }
            } else {
                box_function(&[Ins::I32Const(element), new], &mut self.f.body);
            }
        }

        /// The binding a named function expression makes for its own name,
        /// which is the one binding of this function whose kind names *this*
        /// function. A `function g(){}` written inside the body names `g`'s
        /// id, not this one, so the test is exact.
        fn self_name(&self) -> Option<ast::BindingId> {
            self.program
                .func(self.id)
                .bindings
                .iter()
                .copied()
                .find(|b| self.program.binding(*b).kind == ast::BindingKind::Function(self.id))
        }

        fn function(mut self) -> Result<FnBuild, CompileError> {
            self.open_cells();
            // ECMA-262 15.2.5 step 4 binds a named function expression's own
            // name inside the function, before the body runs.
            //
            // DIVERGENCE, and it is this fix's own: the spec initialises that
            // binding to *the object 15.2.5 just created*, and this
            // initialises it to a fresh one per call. There is no channel to
            // the other: the object was built in the enclosing frame, and
            // without a closure environment nothing carries it in. So `me`
            // is a function, `typeof me` is `"function"`, `me()` recurses and
            // `me === undefined` is false -- everything the binding exists
            // for -- and `f === f()` where the body returns `me` answers
            // `false` where JavaScript answers `true`. The fix is a callee
            // slot in the frame, which is the same machinery `this` needs;
            // `a_named_function_expression_sees_a_function_but_not_its_own
            // _object` in `function_conformance.rs` is where it is written
            // down.
            if let Some(binding) = self.self_name() {
                self.instantiate(binding, self.id);
            }
            let entry = self.id == ast::Program::SCRIPT;
            if entry {
                // The fault word describes *this* call, so the entry point
                // clears it before anything can write one. Without this a
                // heap exhaustion recorded by an earlier call would still be
                // sitting there when a later call trapped for its own,
                // entirely different reason.
                runtime::clear_fault(&mut self.f.body);
                // And the same argument, one word over. The in-flight flag is
                // a module **global**, so it is instance state, and an
                // uncaught throw traps with it still raised -- a tinyvm
                // instance is persistent and a top-level call is the unit of
                // budget, so the next `invoke_by_name` on that instance would
                // begin with a throw already in flight. It read as a `catch`
                // firing in a call whose every path contains no `throw`, bound
                // to the previous call's value -- a pointer into the previous
                // call's heap, where the value was an Object. Two instructions,
                // paid only by a module that already carries the channel.
                if let Some(unwind) = self.unwind {
                    self.push(Ins::I32Const(0));
                    self.push(Ins::GlobalSet(unwind.flag));
                }
                // `JSON`, built the first time this instance is called and
                // read from a global for the rest of its life. `TAG_UNDEFINED`
                // is 0 and the globals start zeroed, so "has it been built" is
                // the tag being undefined -- no second flag, and no cost at
                // all to a program that never names it. See [`Json`].
                if let Some(json) = self.json {
                    self.push(Ins::GlobalGet(json.tag));
                    self.push(Ins::I32Eqz);
                    self.push(Ins::If(BlockType::Empty));
                    box_object(
                        &[
                            Ins::I32Const(json.stringify_element),
                            Ins::I32Const(json.parse_element),
                            Ins::Call(json.ns),
                        ],
                        &mut self.f.body,
                    );
                    self.push(Ins::GlobalSet(json.payload));
                    self.push(Ins::GlobalSet(json.tag));
                    self.push(Ins::End);
                }
            }
            // A throw that reaches the entry point has nowhere to be handed
            // to: every other function answers an uncaught throw by returning
            // and letting its caller's check see the flag, and the script's
            // caller is the host, which is not watching a global. So the
            // script alone opens one block for it and registers it as the
            // outermost handler; what follows the block is the fault the host
            // *can* see.
            let uncaught = entry && self.unwind.is_some();
            if uncaught {
                self.push(Ins::Block(BlockType::Empty));
                self.handlers.push(self.depth);
            }
            self.stmts(&self.program.func(self.id).body)?;
            // Falling off the end. The script yields its completion value --
            // which is `undefined` unless a statement produced one, and is
            // already `undefined` because a fresh local is zeroed and
            // `TAG_UNDEFINED` is 0. Any other function yields `undefined`.
            match self.completion {
                Some(base) => load_local(base, &mut self.f.body),
                None => const_undefined(&mut self.f.body),
            }
            if uncaught {
                // The ordinary end of the script is a `return`, so the
                // epilogue below is reached only by a branch to the block.
                self.push(Ins::Return);
                self.push(Ins::End);
                self.handlers.pop();
                // Which of the three things this is, before the trap that
                // leaves no instruction to say it -- see
                // [`runtime::FAULT_UNCAUGHT_THROW`], which `crate::guest_fault`
                // now reads back as [`crate::GuestFault::UncaughtThrow`].
                runtime::record_uncaught_throw(&mut self.f.body);
                self.push(Ins::Unreachable);
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

        /// Emit one instruction, keeping [`Lower::depth`] with it.
        ///
        /// The count is here and not at the call sites because a branch whose
        /// target is not local reads it, and a single missed `end` would aim
        /// every one of those branches at the wrong label -- silently, since
        /// the module would still be well-typed.
        fn push(&mut self, ins: Ins) {
            match ins {
                Ins::Block(_) | Ins::Loop(_) | Ins::If(_) => self.depth += 1,
                Ins::End => self.depth -= 1,
                _ => {}
            }
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

        /// Where one binding's storage is, from this function's point of view.
        ///
        /// Four answers now, and the two new ones are both about capture: a
        /// binding this function owns *and* something nested reads lives in a
        /// cell whose pointer is in a local; a binding an enclosing function
        /// owns is reached through this function's environment.
        fn place(&self, id: ast::BindingId) -> Place {
            let binding = self.program.binding(id);
            if binding.func == ast::Program::SCRIPT {
                Place::Global(BINDING_GLOBALS + binding.slot * WIDTH)
            } else if binding.func == self.id {
                let base = self.env_param + binding.slot * WIDTH;
                if binding.captured {
                    Place::Cell(base)
                } else {
                    Place::Local(base)
                }
            } else {
                Place::Env(self.capture_index(id))
            }
        }

        /// This binding's index in the environment, which is its position in
        /// `Function::captures` -- the layout the parser's `record_captures`
        /// pass fixed and every creator of this function fills in that order.
        fn capture_index(&self, id: ast::BindingId) -> u32 {
            self.program
                .func(self.id)
                .captures
                .iter()
                .position(|c| *c == id)
                .expect("a Res::Captured occurrence is in its function's capture list")
                as u32
        }

        /// Box every binding this function owns that something nested reads.
        ///
        /// Runs before anything else in the body, because a capture can be
        /// read by a function instantiated at the very top of it.
        ///
        /// A captured **parameter** is the case with an order to get right:
        /// its value arrives in the pair of locals whose `i32` half is about
        /// to become the cell pointer, so the pair is read into the cell
        /// *before* the pointer overwrites it. Reading after would store the
        /// address of the cell into the cell.
        ///
        /// A captured body binding starts as `undefined`, which is what a
        /// zeroed cell already is: `TAG_UNDEFINED` is 0 and so is the payload,
        /// and `__alloc` hands out zeroed memory. So nothing is written for
        /// one -- the same reason a fresh local needs no initialiser.
        fn open_cells(&mut self) {
            if self.id == ast::Program::SCRIPT {
                // The script's bindings are globals; they outlive every frame
                // already, which is why reading one from a nested function is
                // `Res::Global` and not a capture at all.
                return;
            }
            let function = self.program.func(self.id);
            let params = function.params.len() as u32;
            let owned: Vec<(u32, bool)> = function
                .bindings
                .iter()
                .filter(|id| self.program.binding(**id).captured)
                .map(|id| {
                    let slot = self.program.binding(*id).slot;
                    (slot, slot < params)
                })
                .collect();
            for (slot, is_param) in owned {
                let base = self.env_param + slot * WIDTH;
                let cell = self.take_raw();
                self.push(Ins::I32Const(CELL_BYTES));
                let alloc = self.ctx.call(Rt::Alloc);
                self.push(alloc);
                self.push(Ins::LocalSet(cell));
                if is_param {
                    self.push(Ins::LocalGet(cell));
                    self.push(Ins::LocalGet(base));
                    self.push(Ins::I32Store(ALIGN_WORD, CELL_TAG));
                    self.push(Ins::LocalGet(cell));
                    self.push(Ins::LocalGet(base + 1));
                    self.push(Ins::I64Store(ALIGN_WORD, CELL_PAYLOAD));
                }
                self.push(Ins::LocalGet(cell));
                self.push(Ins::LocalSet(base));
                self.give_raw(cell);
            }
        }

        /// Build the environment `callee` expects and leave its pointer on the
        /// stack.
        ///
        /// One allocation and one store per entry, filled from wherever *this*
        /// function keeps that cell: its own local if it declared the binding,
        /// its own environment if it captured it too. That second case is what
        /// makes the closures flat -- a function three levels down is handed
        /// the cell rather than a chain to walk.
        fn build_env(&mut self, callee: ast::FuncId) {
            let captures = self.program.func(callee).captures.clone();
            let env = self.take_raw();
            self.push(Ins::I32Const(captures.len() as i32 * ENV_SLOT));
            let alloc = self.ctx.call(Rt::Alloc);
            self.push(alloc);
            self.push(Ins::LocalSet(env));
            for (index, id) in captures.iter().enumerate() {
                self.push(Ins::LocalGet(env));
                let place = self.place(*id);
                self.cell_pointer(place);
                self.push(Ins::I32Store(ALIGN_WORD, index as u32 * ENV_SLOT as u32));
            }
            self.push(Ins::LocalGet(env));
            self.give_raw(env);
        }

        /// Leave the cell pointer for `place` on the stack.
        fn cell_pointer(&mut self, place: Place) {
            match place {
                Place::Cell(base) => self.push(Ins::LocalGet(base)),
                // Environment is always wasm local 0 of a capturing function.
                Place::Env(index) => {
                    self.push(Ins::LocalGet(0));
                    self.push(Ins::I32Load(ALIGN_WORD, index * ENV_SLOT as u32));
                }
                other => unreachable!("{other:?} is not a cell"),
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
                // Two loads through the cell, tag then payload -- the pair as
                // it was stored, not re-boxed.
                Place::Cell(_) | Place::Env(_) => {
                    self.cell_pointer(place);
                    let cell = self.take_raw();
                    self.push(Ins::LocalSet(cell));
                    self.push(Ins::LocalGet(cell));
                    self.push(Ins::I32Load(ALIGN_WORD, CELL_TAG));
                    self.push(Ins::LocalGet(cell));
                    self.push(Ins::I64Load(ALIGN_WORD, CELL_PAYLOAD));
                    self.give_raw(cell);
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
                // Through the cell, which is what makes capture by *binding*
                // rather than by value: the declaring function writes where
                // every closure over it reads.
                Place::Cell(_) | Place::Env(_) => {
                    let pair = self.take();
                    store_local(pair, &mut self.f.body);
                    self.cell_pointer(place);
                    let cell = self.take_raw();
                    self.push(Ins::LocalSet(cell));
                    self.push(Ins::LocalGet(cell));
                    self.push(Ins::LocalGet(pair));
                    self.push(Ins::I32Store(ALIGN_WORD, CELL_TAG));
                    self.push(Ins::LocalGet(cell));
                    self.push(Ins::LocalGet(pair + 1));
                    self.push(Ins::I64Store(ALIGN_WORD, CELL_PAYLOAD));
                    self.give_raw(cell);
                    self.give(pair);
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
                ast::StmtKind::Block(stmts) => self.stmts(stmts),
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
                    self.finish_return();
                    Ok(())
                }
                ast::StmtKind::Throw(value) => self.throw_stmt(value),
                ast::StmtKind::Try {
                    block,
                    handler,
                    finalizer,
                } => self.try_stmt(block, handler.as_ref(), finalizer.as_deref()),
                // Hoisted before the program was walked: a declaration binds a
                // name to a function index, and a function index needs no
                // storage and nothing to run.
                ast::StmtKind::Func { .. } => Ok(()),
            }
        }

        fn declarator(&mut self, declarator: &ast::Declarator) -> Result<(), CompileError> {
            let binding = self.program.binding(declarator.binding);
            match (&declarator.init, binding.kind) {
                // `const f = function () {}`: 15.2.5 evaluates the
                // FunctionExpression *here*, so the object is built here and
                // not hoisted -- which is what makes a declarator inside a
                // loop body a new function on every pass. `instantiate`
                // decides whether an object is needed at all.
                (
                    Some(ast::Expr {
                        kind: ast::ExprKind::Function(func),
                        ..
                    }),
                    ast::BindingKind::Function(_),
                ) => {
                    debug_assert_eq!(
                        self.program.binding(declarator.binding).kind,
                        ast::BindingKind::Function(*func),
                        "the parser binds the name to the function it was written with"
                    );
                    self.instantiate(declarator.binding, *func);
                    Ok(())
                }
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

        // -- unwinding -------------------------------------------------------

        /// Where a throw goes from here: the nearest enclosing handler, or the
        /// function's own label, which is a `return`.
        ///
        /// One number and not two cases, because `br self.depth` *is* the
        /// return: the function body is a block whose label carries the
        /// function's results, so branching to it hands back whatever pair is
        /// on top of the stack. At a throw check that pair is the callee's
        /// own, which is why the check needs nothing built to satisfy it.
        fn unwind_target(&self) -> u32 {
            match self.handlers.last() {
                Some(&at) => self.depth - at,
                None => self.depth,
            }
        }

        /// The check after a call that could have thrown: two instructions,
        /// and none at all in a program with no `throw` in it.
        fn throw_check(&mut self) {
            let Some(unwind) = self.unwind else {
                return;
            };
            self.push(Ins::GlobalGet(unwind.flag));
            let target = self.unwind_target();
            self.push(Ins::BrIf(target));
        }

        /// `throw e`, ECMA-262 14.14.1: evaluate, then leave.
        fn throw_stmt(&mut self, value: &ast::Expr) -> Result<(), CompileError> {
            let unwind = self
                .unwind
                .expect("the scan sets `throws` for every `throw` in the tree");
            self.expr(value)?;
            // The pair comes off payload first.
            self.push(Ins::GlobalSet(unwind.payload));
            self.push(Ins::GlobalSet(unwind.tag));
            self.push(Ins::I32Const(1));
            self.push(Ins::GlobalSet(unwind.flag));
            self.leave_with_throw();
            Ok(())
        }

        /// Hand a throw whose globals are already set to the nearest handler,
        /// or out of the function.
        fn leave_with_throw(&mut self) {
            match self.handlers.last() {
                Some(&at) => {
                    let back = self.depth - at;
                    self.push(Ins::Br(back));
                }
                // Leaving the function: it owes its caller a pair, and
                // nothing here has one, so it gets `undefined`. The caller
                // reads the flag and never reads the value.
                None => {
                    const_undefined(&mut self.f.body);
                    self.push(Ins::Return);
                }
            }
        }

        /// Perform a `return` of the JS value on top of the stack.
        ///
        /// Straight through when no `finally` stands between here and the
        /// caller. Otherwise the value is parked and the innermost finalizer
        /// is entered with a note to resume the return once it has run, which
        /// is ECMA-262 14.15.3's "if F is not empty" -- and the note is what
        /// lets the finalizer *override* the return by completing abruptly
        /// itself.
        fn finish_return(&mut self) {
            match self.finalizers.last().copied() {
                None => self.push(Ins::Return),
                Some(f) => {
                    store_local(f.slot, &mut self.f.body);
                    self.push(Ins::I32Const(PENDING_RETURN));
                    self.push(Ins::LocalSet(f.pending));
                    let back = self.depth - f.depth;
                    self.push(Ins::Br(back));
                }
            }
        }

        /// Enter a `catch` clause: the throw stops being in flight, and the
        /// value it carried becomes the parameter (ECMA-262 14.15.3 step 4).
        fn bind_caught(&mut self, param: Option<ast::BindingId>) {
            match self.unwind {
                Some(unwind) => {
                    self.push(Ins::I32Const(0));
                    self.push(Ins::GlobalSet(unwind.flag));
                    if let Some(id) = param {
                        let place = self.place(id);
                        self.push(Ins::GlobalGet(unwind.tag));
                        self.push(Ins::GlobalGet(unwind.payload));
                        self.store(place);
                    }
                }
                // A `catch` in a program with no `throw` in it. The clause is
                // unreachable -- nothing can set a flag that does not exist --
                // but it is still code in the module, and its parameter still
                // has to hold something.
                None => {
                    if let Some(id) = param {
                        let place = self.place(id);
                        const_undefined(&mut self.f.body);
                        self.store(place);
                    }
                }
            }
        }

        /// `try`/`catch`/`finally`, ECMA-262 14.15.
        fn try_stmt(
            &mut self,
            block: &[ast::Stmt],
            handler: Option<&ast::Catch>,
            finalizer: Option<&[ast::Stmt]>,
        ) -> Result<(), CompileError> {
            self.reset_completion();
            match finalizer {
                None => self.try_catch(
                    block,
                    handler.expect("the parser refuses a `try` with neither clause"),
                ),
                Some(fin) => self.try_finally(block, handler, fin),
            }
        }

        /// `try { A } catch (e) { B }`, which is the two-block form this
        /// module's header gives for `if`/`else` with the test replaced by a
        /// branch out of the try body.
        ///
        /// ```text
        /// block                  ;; the exit label
        ///   block                ;; the handler: a throw inside A branches here
        ///     <A>
        ///     br 1               ;; A finished: skip the catch
        ///   end
        ///   <bind e, clear the flag>
        ///   <B>
        /// end
        /// ```
        fn try_catch(
            &mut self,
            block: &[ast::Stmt],
            catch: &ast::Catch,
        ) -> Result<(), CompileError> {
            self.push(Ins::Block(BlockType::Empty));
            let exit = self.depth;
            self.push(Ins::Block(BlockType::Empty));
            self.handlers.push(self.depth);
            self.stmts(block)?;
            self.handlers.pop();
            let back = self.depth - exit;
            self.push(Ins::Br(back));
            self.push(Ins::End);
            // 14.15.3: the catch block is *not* inside its own try, so the
            // handler is popped before it is lowered and a `throw` here
            // reaches whatever encloses the whole statement.
            self.bind_caught(catch.param);
            self.stmts(&catch.body)?;
            self.push(Ins::End);
            Ok(())
        }

        /// `try { A } [catch (e) { B }] finally { C }`.
        ///
        /// Three paths leave A -- it finishes, it returns, it throws -- and C
        /// runs on all three and then resumes whichever it was. So they
        /// converge on one copy of C and a `pending` local carries the answer
        /// across it:
        ///
        /// ```text
        /// pending := none
        /// block                       ;; after
        ///   block                     ;; fin: C follows this end
        ///     block                   ;; rethrow: a throw out of B lands here
        ///       block                 ;; handler: a throw out of A lands here
        ///         <A>                 ;; return in A: park, pending := return, br fin
        ///         br fin
        ///       end
        ///       <bind e>; <B>         ;; a throw out of B branches to rethrow
        ///       br fin
        ///     end
        ///     <park the thrown value; pending := throw; clear the flag>
        ///   end
        ///   <C>                       ;; enclosing handler/finalizer, not this one
        ///   <resume pending>
        /// end
        /// ```
        ///
        /// The `rethrow` block is absent when there is no `catch`, because
        /// then the handler's continuation already *is* the parking code.
        ///
        /// Two details are semantics and not shape. The thrown value is
        /// parked in a local rather than left in [`Unwind`]'s globals,
        /// because C may call a function that throws and catches internally
        /// and would overwrite them. And C is lowered with this try's handler
        /// and finalizer already popped, which is what makes an abrupt
        /// completion of C *replace* the pending one -- 14.15.3's last step,
        /// and the reason `try { return 1; } finally { return 2; }` is 2.
        fn try_finally(
            &mut self,
            block: &[ast::Stmt],
            handler: Option<&ast::Catch>,
            finalizer: &[ast::Stmt],
        ) -> Result<(), CompileError> {
            let pending = self.take_raw();
            let slot = self.take();
            self.push(Ins::I32Const(PENDING_NONE));
            self.push(Ins::LocalSet(pending));

            self.push(Ins::Block(BlockType::Empty));
            let after = self.depth;
            self.push(Ins::Block(BlockType::Empty));
            let fin = self.depth;
            self.finalizers.push(Finalizer {
                depth: fin,
                pending,
                slot,
            });

            if let Some(catch) = handler {
                self.push(Ins::Block(BlockType::Empty));
                let rethrow = self.depth;
                self.push(Ins::Block(BlockType::Empty));
                self.handlers.push(self.depth);
                self.stmts(block)?;
                self.handlers.pop();
                let back = self.depth - fin;
                self.push(Ins::Br(back));
                self.push(Ins::End);
                self.handlers.push(rethrow);
                self.bind_caught(catch.param);
                self.stmts(&catch.body)?;
                self.handlers.pop();
                let back = self.depth - fin;
                self.push(Ins::Br(back));
                self.push(Ins::End);
            } else {
                self.push(Ins::Block(BlockType::Empty));
                self.handlers.push(self.depth);
                self.stmts(block)?;
                self.handlers.pop();
                let back = self.depth - fin;
                self.push(Ins::Br(back));
                self.push(Ins::End);
            }

            // Reached only by a throw that nothing above caught. Park it and
            // fall through into C -- `br 0` here would be the same
            // instruction as falling off the end of this block, so there is
            // none.
            self.park_pending_throw(slot, pending);
            self.finalizers.pop();
            self.push(Ins::End);

            // 14.15.3 step 3: *if F is a normal completion, set F to B.* A
            // finalizer that finishes normally contributes nothing at all to
            // the value -- not even its own -- so `try { 1; } finally { 2; }`
            // is `1`. C is an ordinary statement list here and an expression
            // statement in it writes the completion slot, so B is held across
            // C and put back. Only the normal path reads the slot again: the
            // two abrupt ones carry their value in [`Finalizer::slot`], which
            // is why the restore needs no guard.
            //
            // The save is a second scratch pair and not [`Finalizer::slot`],
            // which is already spoken for by the pending return or throw.
            let held = self.completion.map(|base| {
                let held = self.take();
                load_local(base, &mut self.f.body);
                store_local(held, &mut self.f.body);
                held
            });
            self.stmts(finalizer)?;
            if let (Some(base), Some(held)) = (self.completion, held) {
                load_local(held, &mut self.f.body);
                store_local(base, &mut self.f.body);
                self.give(held);
            }
            self.resume_pending(after, slot, pending);
            self.push(Ins::End);

            self.give(slot);
            self.give_raw(pending);
            Ok(())
        }

        /// Take the throw out of flight and hold it across the finalizer.
        fn park_pending_throw(&mut self, slot: u32, pending: u32) {
            let Some(unwind) = self.unwind else {
                // No `throw` in the program: this path is unreachable, and
                // the locals it would have written are never read.
                return;
            };
            self.push(Ins::GlobalGet(unwind.tag));
            self.push(Ins::GlobalGet(unwind.payload));
            store_local(slot, &mut self.f.body);
            self.push(Ins::I32Const(0));
            self.push(Ins::GlobalSet(unwind.flag));
            self.push(Ins::I32Const(PENDING_THROW));
            self.push(Ins::LocalSet(pending));
        }

        /// After the finalizer: do what the path that entered it was doing.
        ///
        /// Emitted with this try's handler and finalizer already popped, so
        /// both the resumed `return` and the resumed throw ask the *enclosing*
        /// question -- which is how a `return` walks every finalizer between
        /// it and the caller.
        fn resume_pending(&mut self, after: u32, slot: u32, pending: u32) {
            // Nothing pending: leave.
            self.push(Ins::LocalGet(pending));
            self.push(Ins::I32Eqz);
            let out = self.depth - after;
            self.push(Ins::BrIf(out));

            self.push(Ins::LocalGet(pending));
            self.push(Ins::I32Const(PENDING_RETURN));
            self.push(Ins::I32Eq);
            self.push(Ins::If(BlockType::Empty));
            load_local(slot, &mut self.f.body);
            self.finish_return();
            self.push(Ins::End);

            // What is left is a pending throw. Put it back in flight and hand
            // it on.
            if let Some(unwind) = self.unwind {
                load_local(slot, &mut self.f.body);
                self.push(Ins::GlobalSet(unwind.payload));
                self.push(Ins::GlobalSet(unwind.tag));
                self.push(Ins::I32Const(1));
                self.push(Ins::GlobalSet(unwind.flag));
                self.leave_with_throw();
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
                // One numeric type, so a fraction lowers exactly as an
                // integer does -- the literal's only job was to say which
                // double.
                ast::ExprKind::Num(value) => {
                    const_number(*value, &mut self.f.body);
                    Ok(())
                }
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
                ast::ExprKind::Name(name) => self.name(name),
                ast::ExprKind::Object(properties) => self.object_literal(properties),
                ast::ExprKind::Array(elements) => self.array_literal(elements),
                // 13.3.2.1 and 13.3.3.1 both evaluate the object first and the
                // key second, which is exactly the order the operands have to
                // reach the accessor in -- so a member read needs no scratch
                // local at all, under either spelling.
                ast::ExprKind::Member { object, key } => {
                    self.expr(object)?;
                    match self.accessor(key) {
                        Accessor::Obj => {
                            self.key(key)?;
                            let call = self.ctx.call(Rt::ObjGet);
                            self.push(call);
                        }
                        Accessor::Prop(base) => {
                            self.key_pair(key)?;
                            self.push(Ins::Call(base + Ar::PropGet.offset()));
                        }
                    }
                    Ok(())
                }
                // A function expression reached here rather than as a
                // callee, so ECMA-262 15.2.5 runs and its answer is a new
                // object every time this expression is evaluated.
                ast::ExprKind::Function(id) => {
                    self.function_value(*id);
                    Ok(())
                }
                ast::ExprKind::Call { callee, args } => self.call(callee, args),
                ast::ExprKind::Unary(op, operand) => self.unary(*op, operand),
                ast::ExprKind::Update { op, prefix, target } => self.update(*op, *prefix, target),
                ast::ExprKind::Binary(op, lhs, rhs) => {
                    self.expr(lhs)?;
                    self.expr(rhs)?;
                    let call = self.ctx.call(binary(*op));
                    self.push(call);
                    Ok(())
                }
                ast::ExprKind::Conditional { test, then, alt } => self.conditional(test, then, alt),
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

        /// Build one ArrayLiteral, ECMA-262 13.2.4.
        ///
        /// The vector is allocated at the literal's exact length, so a literal
        /// never reallocates -- the same reason `__obj_new` takes a count.
        ///
        /// The pointer lives in a *raw* local rather than a value local while
        /// the elements are evaluated, because `__arr_push` takes the pointer
        /// and not the pair: keeping the boxed form would mean unboxing it
        /// once per element to hand back the same `i32` the allocator just
        /// returned.
        fn array_literal(&mut self, elements: &[ast::Expr]) -> Result<(), CompileError> {
            let base = self
                .arrays
                .expect("the scan sets `arrays` for any program with an array literal");
            let raw = self.take_raw();
            self.push(Ins::I32Const(elements.len() as i32));
            self.push(Ins::Call(base + Ar::New.offset()));
            self.push(Ins::LocalSet(raw));
            for element in elements {
                self.push(Ins::LocalGet(raw));
                self.expr(element)?;
                self.push(Ins::Call(base + Ar::Push.offset()));
            }
            box_array(&[Ins::LocalGet(raw)], &mut self.f.body);
            self.give_raw(raw);
            Ok(())
        }

        /// Which accessor a member expression reaches: the dispatcher when
        /// this program can hold an array, and the object accessors when it
        /// cannot.
        ///
        /// # This split used to be Static-vs-Computed, and that was wrong
        ///
        /// `plan/design-array-milestone.md` §2.2 argued that a Static key
        /// could keep the pre-array path unconditionally, "because an
        /// IdentifierName is never a canonical numeric string, so `o.a` can
        /// never be an index". Both halves are true and the conclusion does
        /// not follow: **`a.length` is a Static key on an array**, and it does
        /// not want to be an index -- it wants the header word. Under the
        /// narrower rule it reached `__obj_get`, whose receiver test is
        /// `unbox_object`, and `[1,2,3].length` trapped. Measured, not
        /// reasoned: it is what the first end-to-end probe of this milestone
        /// printed.
        ///
        /// So the gate is the program, not the node. What the design note
        /// promised is still kept, because what it promised was that *a
        /// program with no array pays nothing* -- and one still pays nothing:
        /// no set, no dispatcher, byte-identical output. What it also implied,
        /// that an array-using program's dotted object accesses stay free,
        /// was never load-bearing and is now false by two tag tests: one for
        /// the Object arm that returns immediately, and one inside
        /// `__to_string`, which answers a String with itself.
        ///
        /// Still not a per-call-site exemption, which is the rule
        /// [`Lower::key`] states: this asks a property of the whole program,
        /// never a guess about one receiver's type.
        fn accessor(&self, _key: &ast::MemberKey) -> Accessor {
            match self.arrays {
                Some(base) => Accessor::Prop(base),
                None => Accessor::Obj,
            }
        }

        /// Leave one JS *value* on the stack: the key, unconverted.
        ///
        /// The pair-shaped counterpart of [`Lower::key`], for the dispatcher.
        /// The two arms are still ECMA-262's two and in the same relation --
        /// 13.3.2.1's key is the String value of the IdentifierName, so the
        /// Static arm is a String constant and has run ToPropertyKey by
        /// construction; 13.3.3.1's is an expression whose ToPropertyKey has
        /// not run yet, and `__prop_get` is what decides whether it needs to.
        /// Deferring that decision is the whole point: an index reaching
        /// `__to_string` first would be decimal digits before anything could
        /// notice it was a Number.
        fn key_pair(&mut self, key: &ast::MemberKey) -> Result<(), CompileError> {
            match key {
                ast::MemberKey::Static(name) => {
                    let pointer = self.pool.intern(name);
                    const_string(pointer, &mut self.f.body);
                }
                ast::MemberKey::Computed(expr) => self.expr(expr)?,
            }
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
                    let call = self.ctx.call(Rt::ToStr);
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
                    match key {
                        TargetKey::Raw(raw) => {
                            self.push(Ins::LocalGet(raw));
                            let call = self.ctx.call(Rt::ObjGet);
                            self.push(call);
                        }
                        TargetKey::Pair { slot, base } => {
                            load_local(slot, &mut self.f.body);
                            self.push(Ins::Call(base + Ar::PropGet.offset()));
                        }
                    }
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
                    match key {
                        TargetKey::Raw(raw) => {
                            self.push(Ins::LocalGet(raw));
                            load_local(slot, &mut self.f.body);
                            let call = self.ctx.call(Rt::ObjSet);
                            self.push(call);
                        }
                        TargetKey::Pair {
                            slot: key_slot,
                            base,
                        } => {
                            load_local(key_slot, &mut self.f.body);
                            load_local(slot, &mut self.f.body);
                            self.push(Ins::Call(base + Ar::PropSet.offset()));
                        }
                    }
                }
            }
        }

        /// Give back the scratch a member target held, innermost first.
        fn release(&mut self, target: Target) {
            if let Target::Member { object, key } = target {
                match key {
                    TargetKey::Raw(raw) => self.give_raw(raw),
                    TargetKey::Pair { slot, .. } => self.give(slot),
                }
                self.give(object);
            }
        }

        fn name(&mut self, name: &ast::Name) -> Result<(), CompileError> {
            match &name.res {
                ast::Res::Local(id) | ast::Res::Global(id) | ast::Res::Captured(id) => {
                    let place = self.place(*id);
                    self.load(place);
                    Ok(())
                }
                // As at M0: a bare host name is the zero-argument call.
                ast::Res::Host(text) => self.host_call(text, &[]),
                // A name bound to a known function, read rather than called.
                // An ordinary storage read: the object was built once, when
                // the scope holding the binding was entered (a declaration) or
                // when the declarator ran (`const f = function () {}`), and
                // reading the name twice has to give the same object back.
                // This used to rebuild the value from the element index, which
                // is what made two *evaluations* one function.
                ast::Res::Callee(id) => {
                    debug_assert!(
                        self.value_bindings.contains(id),
                        "the scan records every name read as a value"
                    );
                    let place = self.place(*id);
                    self.load(place);
                    Ok(())
                }
                // The engine's own `JSON`: an ordinary read of an ordinary
                // Object, out of the pair the entry prologue filled. Nothing
                // about the *use* of it is special -- `JSON.parse` is a
                // property read and a call through the value it finds, the
                // same two things `o.m()` is.
                ast::Res::Json => {
                    let json = self
                        .json
                        .expect("the scan sets `json` for every occurrence of the name");
                    self.push(Ins::GlobalGet(json.tag));
                    self.push(Ins::GlobalGet(json.payload));
                    Ok(())
                }
                ast::Res::Unresolved => {
                    unreachable!("the parser resolves every occurrence before it returns")
                }
            }
        }

        /// The function a `Callee` binding names. The parser only ever
        /// classifies a [`ast::BindingKind::Function`] binding as one.
        fn func_of(&self, id: ast::BindingId) -> ast::FuncId {
            match self.program.binding(id).kind {
                ast::BindingKind::Function(func) => func,
                _ => unreachable!("the parser only classifies a function binding as a callee"),
            }
        }

        /// A call, in the three shapes ECMA-262 13.3.6.1 collapses into one.
        ///
        /// Three of the four callee forms name a function the compiler already
        /// knows, and each of those is a plain `call` at the exact arity the
        /// callee declares. Everything else -- a property, a call's result, a
        /// name holding a value -- goes through the table.
        fn call(&mut self, callee: &ast::Expr, args: &[ast::Expr]) -> Result<(), CompileError> {
            // Research only -- Q1 variant C. Deleted when the track is decided.
            #[cfg(feature = "method-callsite")]
            if let ast::ExprKind::Member {
                object,
                key: ast::MemberKey::Static(name),
            } = &callee.kind
                && let Some(me) = method::Me::at_call_site(name, args.len())
            {
                return if me == method::Me::Map {
                    self.specialised_map(object, name.clone(), args)
                } else {
                    self.specialised_method(object, name.clone(), me, args)
                };
            }
            let target = match &callee.kind {
                ast::ExprKind::Name(ast::Name {
                    res: ast::Res::Host(text),
                    ..
                }) => return self.host_call(text, args),
                ast::ExprKind::Name(ast::Name {
                    res: ast::Res::Callee(id),
                    ..
                }) => self.func_of(*id),
                // `(function () {})()`.
                ast::ExprKind::Function(func) => *func,
                _ => return self.indirect_call(callee, args),
            };
            // A capturing callee takes its environment first. The caller
            // builds it, because the caller is the one that has the cells --
            // in its own locals if it declared them, in its own environment if
            // it captured them too.
            if !self.program.func(target).captures.is_empty() {
                self.build_env(target);
            }
            let arity = self.program.func(target).params.len() as u32;
            self.arguments(args, arity)?;
            self.push(Ins::Call(self.user_base + target.0));
            // One of the two places a throw can arrive from somewhere else.
            self.throw_check();
            Ok(())
        }

        /// A call through a value: `call_indirect` on the one uniform
        /// signature, with the tag test standing in front of it.
        ///
        /// ```text
        /// <evaluate the callee into a scratch pair>
        /// <evaluate every argument, padded to the uniform arity>
        /// <trap unless the callee's tag is TAG_FUNCTION>
        /// <push the element index>
        /// call_indirect
        /// ```
        ///
        /// The order is 13.3.6.1's, exactly: the callee is evaluated first
        /// (step 1), then every argument (EvaluateCall step 3), and only then
        /// is the callee checked for callability (step 4). So a script whose
        /// arguments have side effects sees them happen even though the call
        /// is about to fail -- which is what the specification says, and is
        /// why the tag test is not hoisted in front of the arguments where it
        /// would read more naturally.
        ///
        /// **No receiver is passed.** 13.3.6.1 step 2 makes `o.m()`'s `this`
        /// the object `o`; this engine has no `this` -- the keyword is a
        /// capability refusal -- so `o.m()` calls the function `o.m` holds and
        /// the function cannot see `o`. That is a real divergence and not an
        /// oversight; it is inert for a method that never writes `this`, which
        /// is every method in the library this milestone exists to compile,
        /// and it is what the `this` milestone has to fix.
        fn indirect_call(
            &mut self,
            callee: &ast::Expr,
            args: &[ast::Expr],
        ) -> Result<(), CompileError> {
            let uniform = self
                .uniform
                .expect("the scan finds every call that is not to a known function");
            let slot = self.take();
            self.expr(callee)?;
            store_local(slot, &mut self.f.body);
            // The environment goes first because the uniform signature leads
            // with it. It comes out of the record, which is what the callee
            // value's payload *is* -- the adapter cannot ask which funcref it
            // was reached through, so the call site that already holds the
            // record is the one place the answer exists.
            if self.captures {
                unbox_function(slot, &mut self.f.body);
                self.push(Ins::I32Load(ALIGN_WORD, FN_ENV));
            }
            self.arguments(args, uniform.arity)?;
            unbox_function(slot, &mut self.f.body);
            self.push(Ins::I32Load(ALIGN_WORD, FN_ELEMENT));
            self.push(Ins::CallIndirect(uniform.type_index, 0));
            // The other one. The adapter in the table needs no check of its
            // own: it forwards and returns, so a throw the target raised is
            // still in flight when this returns.
            self.throw_check();
            self.give(slot);
            Ok(())
        }

        /// Research only -- Q1 variant C, the call-site specialisation itself.
        ///
        /// ```text
        /// <evaluate the receiver once, into a scratch pair>
        /// if the receiver is a String:  call the method's prefab directly
        /// if it is not:                 read the property and call the value
        /// ```
        ///
        /// **The run-time test cannot be skipped**, and that is the variant's
        /// first leak. The source says `x.trim()`; whether `x` is a String or
        /// an object carrying a `trim` property is not knowable until it runs,
        /// and `method_conformance::a_plain_object_property_named_like_a_method_is_untouched`
        /// is the assertion that says the second case must keep working. So
        /// variant C removes the *function value*, not the *dispatch* -- the
        /// branch moves out of the callee and into every call site.
        ///
        /// **The receiver is evaluated once**, which is why this cannot simply
        /// fall through to [`Lower::indirect_call`] on the non-String path:
        /// that re-lowers the callee, and `f().trim()` would call `f` twice.
        /// A second leak, and a more expensive one -- it is what forces the
        /// scratch pair and the two-`If` shape below.
        ///
        /// Two `If`s over a result slot rather than an if/else: this IR has no
        /// `Else` and `BlockType` has only `Empty`, so a branch that produces
        /// a value has to route it through a local. Recorded as a third leak
        /// -- it is a property of the IR, not of the variant, but the variant
        /// is what pays for it.
        #[cfg(feature = "method-callsite")]
        fn specialised_method(
            &mut self,
            object: &ast::Expr,
            name: String,
            me: method::Me,
            args: &[ast::Expr],
        ) -> Result<(), CompileError> {
            let (base, plan) = self
                .methods
                .clone()
                .expect("the scan turns the set on for every specialised call");
            let recv = self.take();
            let result = self.take();
            self.expr(object)?;
            store_local(recv, &mut self.f.body);

            // The typed path: the prefab, called directly, with no value.
            // Which tag is right depends on the method -- `trim` wants a
            // String, `push` an Array -- so the test lives at the call site
            // per method rather than being one shared check.
            if me.receiver_is_array() {
                is_array(recv, &mut self.f.body);
            } else {
                is_string(recv, &mut self.f.body);
            }
            self.push(Ins::If(BlockType::Empty));
            load_local(recv, &mut self.f.body);
            for arg in args {
                self.expr(arg)?;
            }
            self.push(Ins::Call(base + plan.offset(me)));
            store_local(result, &mut self.f.body);
            self.push(Ins::End);

            // Everything else: the ordinary property read and indirect call,
            // on the receiver already in hand.
            if me.receiver_is_array() {
                is_array(recv, &mut self.f.body);
            } else {
                is_string(recv, &mut self.f.body);
            }
            self.push(Ins::I32Eqz);
            self.push(Ins::If(BlockType::Empty));
            let uniform = self
                .uniform
                .expect("the scan finds every call that is not to a known function");
            let value = self.take();
            load_local(recv, &mut self.f.body);
            let key = self.pool.intern(&name);
            self.push(Ins::I32Const(key));
            let get = self.ctx.call(Rt::ObjGet);
            self.push(get);
            store_local(value, &mut self.f.body);
            if self.captures {
                unbox_function(value, &mut self.f.body);
                self.push(Ins::I32Load(ALIGN_WORD, FN_ENV));
            }
            self.arguments(args, uniform.arity)?;
            unbox_function(value, &mut self.f.body);
            self.push(Ins::I32Load(ALIGN_WORD, FN_ELEMENT));
            self.push(Ins::CallIndirect(uniform.type_index, 0));
            store_local(result, &mut self.f.body);
            self.give(value);
            self.push(Ins::End);

            load_local(result, &mut self.f.body);
            self.throw_check();
            self.give(result);
            self.give(recv);
            Ok(())
        }

        /// Research only -- Q1 variant C. `a.map(f)`, with the loop **inlined
        /// at the call site**.
        ///
        /// Every other method here is a direct call to a prefab. `map` cannot
        /// be, and the reason is the sharpest structural fact this experiment
        /// has turned up: a prefab would have to `call_indirect` into the
        /// callback, and **no runtime prefab in this compiler can** -- the
        /// only `call_indirect` in the tree is the one a few functions below.
        /// Variant C sidesteps that by emitting the loop here, where the
        /// instruction is in scope. Variants A and B cannot sidestep it: for
        /// them the method *is* a value, so its body is a function, and that
        /// function needs exactly the ability this one avoids needing.
        ///
        /// The price is paid in criterion ③b. Every other method costs ~43
        /// bytes per call site; this one costs a whole loop per call site.
        #[cfg(feature = "method-callsite")]
        fn specialised_map(
            &mut self,
            object: &ast::Expr,
            name: String,
            args: &[ast::Expr],
        ) -> Result<(), CompileError> {
            let arrays = self
                .arrays
                .expect("a `map` call site turns the array gate on");
            let uniform = self
                .uniform
                .expect("the scan finds every call that is not to a known function");
            let recv = self.take();
            let callback = self.take();
            let elem = self.take();
            let result = self.take();
            let a = self.take_raw();
            let out = self.take_raw();
            let i = self.take_raw();
            let n = self.take_raw();

            self.expr(object)?;
            store_local(recv, &mut self.f.body);
            self.expr(&args[0])?;
            store_local(callback, &mut self.f.body);

            is_array(recv, &mut self.f.body);
            self.push(Ins::If(BlockType::Empty));
            unbox_array(recv, &mut self.f.body);
            self.push(Ins::LocalSet(a));
            self.push(Ins::LocalGet(a));
            self.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
            self.push(Ins::LocalSet(n));
            self.push(Ins::LocalGet(n));
            self.push(Ins::Call(arrays + Ar::New.offset()));
            self.push(Ins::LocalSet(out));
            self.push(Ins::I32Const(0));
            self.push(Ins::LocalSet(i));
            self.push(Ins::Block(BlockType::Empty));
            self.push(Ins::Loop(BlockType::Empty));
            self.push(Ins::LocalGet(i));
            self.push(Ins::LocalGet(n));
            self.push(Ins::I32GeU);
            self.push(Ins::BrIf(1));
            self.push(Ins::LocalGet(a));
            self.push(Ins::LocalGet(i));
            self.push(Ins::Call(arrays + Ar::Get.offset()));
            store_local(elem, &mut self.f.body);
            if self.captures {
                unbox_function(callback, &mut self.f.body);
                self.push(Ins::I32Load(ALIGN_WORD, FN_ENV));
            }
            load_local(elem, &mut self.f.body);
            for _ in 1..uniform.arity {
                const_undefined(&mut self.f.body);
            }
            unbox_function(callback, &mut self.f.body);
            self.push(Ins::I32Load(ALIGN_WORD, FN_ELEMENT));
            self.push(Ins::CallIndirect(uniform.type_index, 0));
            store_local(elem, &mut self.f.body);
            self.push(Ins::LocalGet(out));
            load_local(elem, &mut self.f.body);
            self.push(Ins::Call(arrays + Ar::Push.offset()));
            self.push(Ins::LocalGet(i));
            self.push(Ins::I32Const(1));
            self.push(Ins::I32Add);
            self.push(Ins::LocalSet(i));
            self.push(Ins::Br(0));
            self.push(Ins::End);
            self.push(Ins::End);
            let mut inner = Vec::new();
            inner.push(Ins::LocalGet(out));
            let mut boxed = Vec::new();
            box_array(&inner, &mut boxed);
            self.f.body.extend(boxed);
            store_local(result, &mut self.f.body);
            self.push(Ins::End);

            // A receiver that is not an Array: the ordinary property read and
            // indirect call, on the receiver already in hand.
            is_array(recv, &mut self.f.body);
            self.push(Ins::I32Eqz);
            self.push(Ins::If(BlockType::Empty));
            let value = self.take();
            load_local(recv, &mut self.f.body);
            let key = self.pool.intern(&name);
            self.push(Ins::I32Const(key));
            let get = self.ctx.call(Rt::ObjGet);
            self.push(get);
            store_local(value, &mut self.f.body);
            if self.captures {
                unbox_function(value, &mut self.f.body);
                self.push(Ins::I32Load(ALIGN_WORD, FN_ENV));
            }
            self.arguments(args, uniform.arity)?;
            unbox_function(value, &mut self.f.body);
            self.push(Ins::I32Load(ALIGN_WORD, FN_ELEMENT));
            self.push(Ins::CallIndirect(uniform.type_index, 0));
            store_local(result, &mut self.f.body);
            self.give(value);
            self.push(Ins::End);

            load_local(result, &mut self.f.body);
            self.throw_check();
            self.give_raw(n);
            self.give_raw(i);
            self.give_raw(out);
            self.give_raw(a);
            self.give(result);
            self.give(elem);
            self.give(callback);
            self.give(recv);
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
        /// argument is `undefined` (ECMA-262 8.6.1), and a surplus one is
        /// still *evaluated* -- it can assign, or call -- and only then
        /// discarded (13.3.8.1 evaluates the whole list; 10.2.11 binds what
        /// the function declares).
        ///
        /// `arity` is the callee's own for a direct call and the uniform one
        /// for a call through the table, where the adapter does the second
        /// half of the same reconciliation.
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

        /// `test ? then : alt`, ECMA-262 13.14.
        ///
        /// The two-block form this module's header gives for `if`/`else`,
        /// with a scratch local carrying the value out for the reason
        /// [`Lower::logical`] gives: `repr`'s `BlockType` has only `Empty`, so
        /// a block that yielded a JS value would need a multi-value type
        /// index.
        ///
        /// 13.14.1 evaluates the test, runs ToBoolean on it, and then
        /// evaluates **one** branch. That is not a saving, it is the meaning:
        /// a lowering that evaluated both and selected afterwards would run
        /// the side effects of the branch the test rejected, and would answer
        /// every value question correctly while doing it. `__truthy` is the
        /// same ToBoolean `if` runs; there is one of it.
        fn conditional(
            &mut self,
            test: &ast::Expr,
            then: &ast::Expr,
            alt: &ast::Expr,
        ) -> Result<(), CompileError> {
            let slot = self.take();
            self.push(Ins::Block(BlockType::Empty));
            let exit = self.depth;
            self.push(Ins::Block(BlockType::Empty));
            // The test is *inverted*: the branch it takes is the one to the
            // else operand, which is what lets the then operand fall through.
            self.truthy(test)?;
            self.push(Ins::I32Eqz);
            self.push(Ins::BrIf(0));
            self.expr(then)?;
            store_local(slot, &mut self.f.body);
            let back = self.depth - exit;
            self.push(Ins::Br(back));
            self.push(Ins::End);
            self.expr(alt)?;
            store_local(slot, &mut self.f.body);
            self.push(Ins::End);
            load_local(slot, &mut self.f.body);
            self.give(slot);
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
                    res: ast::Res::Local(id) | ast::Res::Global(id) | ast::Res::Captured(id),
                    ..
                }) => Ok(Target::Binding(self.place(*id))),
                ast::ExprKind::Member { object, key } => {
                    let slot = self.take();
                    self.expr(object)?;
                    store_local(slot, &mut self.f.body);
                    let held = match self.accessor(key) {
                        Accessor::Obj => {
                            let raw = self.take_raw();
                            self.key(key)?;
                            self.push(Ins::LocalSet(raw));
                            TargetKey::Raw(raw)
                        }
                        Accessor::Prop(base) => {
                            let key_slot = self.take();
                            self.key_pair(key)?;
                            store_local(key_slot, &mut self.f.body);
                            TargetKey::Pair {
                                slot: key_slot,
                                base,
                            }
                        }
                    };
                    Ok(Target::Member {
                        object: slot,
                        key: held,
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
            ast::ExprKind::Int(_) | ast::ExprKind::Num(_) => "a Number",
            ast::ExprKind::Str(_) => "a String",
            ast::ExprKind::Bool(_) => "a Boolean",
            ast::ExprKind::Object(_) => "an Object",
            // Not an ECMA-262 language type name -- a function is an Object in
            // the specification -- but it is what `typeof` answers and what a
            // reader of the diagnostic wrote.
            ast::ExprKind::Function(_) => "a function",
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
