//! The guest-side runtime the compiler splices into every module.
//!
//! Every operator is a call. `a + b` in a language where `+` dispatches on the
//! operand types is a runtime call, exactly as it is in an unoptimised bytecode
//! engine; inlining it is an optimisation this compiler does not have yet and
//! does not need in order to be correct. The lowering emits the two operands as
//! [`super::repr`] pairs and then one [`Ins::Call`].
//!
//! Lifted from the value-representation experiment's `src/runtime.rs`, with the
//! `Repr` trait erased (V1 won; there is no second variant to parameterise
//! over) and the semantics moved from research-grade to ECMA-262 wherever that
//! cost no new machinery -- see the per-function comments.
//!
//! # Where this stops, and how it says so
//!
//! Three conversions in ECMA-262 need machinery this milestone does not have:
//!
//! - `ToString` of a Number (7.1.17 / 6.1.6.1.20, the Number::toString
//!   algorithm) -- so `"a" + 1` cannot be evaluated.
//! - `StringToNumber` (7.1.4.1, the `StringNumericLiteral` grammar) -- so
//!   `"1" - 1` and `1 == "1"` cannot be evaluated.
//! - String relational comparison by code unit (7.2.13) -- so `"a" < "b"`
//!   cannot be evaluated.
//!
//! Each is an explicit `unreachable` arm rather than a fabricated result. A
//! wrong number that flows on is indistinguishable from a real one; a trap is
//! loud and arrives at the host as a typed fault. The front end cannot name
//! these as capability diagnostics because it does not know the operand types
//! -- that is what a dynamic language means -- so the boundary is enforced at
//! the only place that does know it.
//!
//! # One urge, detected and refused
//!
//! Inside an arm already guarded by `is_number`, `unbox_number` re-tests the
//! tag and traps -- a check that provably cannot fire. Deleting it there is
//! worth four instructions on the hottest path in the engine, and it is not
//! done. An accessor whose safety depends on the caller having checked first
//! is an accessor that is unsafe the day someone calls it from a new arm, and
//! the compiler has no pass that would notice. The value-representation
//! experiment refused the same shape twice (`RESULTS.md`, L2.5 and L2.6); this
//! is the third instance and it is refused on the same grounds. The cure, when
//! it is wanted, is a peephole pass that proves the tag is known -- one place
//! that can be tested -- not an exemption written into the dispatch sites.
//!
//! # Heap
//!
//! A bump allocator with no free and no collector. A string is
//! `[len: i32][utf8 bytes]`, 4-byte aligned, with no interning and no
//! 8/16-bit forms. That is the smallest heap strings can land on; the shape it
//! grows into is a later milestone's decision, and nothing above this file
//! reads the layout except [`super::repr`]'s string pointer.
//!
//! An object is on the same heap -- see [`OBJ_HEADER`] for the record and for
//! why it is a flat key/value vector rather than a shape table.

use super::repr::{
    self, BlockType, Ins, ValType, WIDTH, box_bool, box_number, box_string, const_bool,
    const_string, const_undefined, is_array, is_bool, is_function, is_null, is_nullish, is_number,
    is_object, is_string, is_undefined, load_local, same_type, store_local, unbox_bool,
    unbox_number, unbox_object, unbox_string,
};

/// Byte 0..8 is left out of the data segment so a null pointer is never a
/// valid string. The first word of it is the fault word; the second stays
/// reserved.
pub(crate) const DATA_ORIGIN: u32 = 8;

/// The guest's own account of why it trapped, at a fixed address in its linear
/// memory.
///
/// A refused `memory.grow` is not a trap -- standard wasm has it return `-1`
/// (`crates/tinyvm/src/wasm.rs`, `Op::MemoryGrow`, the `stack.push(Val::I32(-1))`
/// arm) -- so it carries no reason of its own, and the `unreachable` the
/// allocator falls into afterwards is byte-for-byte the same fault a genuine
/// type error produces. A host that saw only the trap would have to *guess*
/// which one it had, and guessing wrong means telling an author their script is
/// broken when the truth is that the heap ran out.
///
/// So the guest writes down what it knows on the way down, at a word no
/// allocation can ever hand out: the bump pointer starts at
/// [`StringPool::heap_start`], which is never below [`DATA_ORIGIN`], and the
/// only instruction in the emitted module that stores here is the one below.
/// Nothing is imported, nothing is exported and no host has to be watching --
/// the record is simply there afterwards, for a host that has the instance.
///
/// Reading it is [`crate::guest_fault`]; that function and this constant are
/// the same fact stated on the two sides of the boundary.
pub(crate) const FAULT_WORD: i32 = 0;

/// This call recorded no fault. Written at the top of the entry point, so the
/// word always describes the call the host just made rather than an older one.
///
/// Only `emit` writes it, and `tests/repr_v1.rs` includes this module without
/// that one.
#[allow(dead_code)]
pub(crate) const FAULT_NONE: i32 = 0;

/// `memory.grow` refused: the bump heap cannot hold what the script asked for.
/// A budget fact, not a defect in the script.
pub(crate) const FAULT_HEAP_EXHAUSTED: i32 = 1;

/// A `throw` reached the host with no `catch` between. Neither a budget fact
/// nor a defect in the engine: the script ran exactly as written, ECMA-262
/// says the program terminates with that exception, and the host's right
/// answer is to report it -- not to raise a memory ceiling and not to tell
/// the author their script is broken.
///
/// It is a *third* code and not a reuse of either existing one for the reason
/// [`FAULT_WORD`] exists at all: a host that cannot tell "your script threw"
/// from "your script is broken" will tell an author the wrong thing. Without
/// it, an uncaught throw would arrive as the same bare `unreachable` a missing
/// conversion executes, which is precisely the misclassification the fault
/// word was added to prevent.
///
/// It does **not** carry the thrown value. A module exports no global, so a
/// host cannot read the unwind channel that holds it; handing it out would
/// mean exporting an engine-internal pair or widening the entry point's
/// results, and both of those are decisions about the host boundary rather
/// than about throwing.
///
/// Two producers, and they must stay one number: `super::convert`'s `__throw`
/// when the module has no unwind channel, and `super::emit`'s entry-point
/// epilogue when a throw reaches the top of the script. [`crate::guest_fault`]
/// names it, as [`crate::GuestFault::UncaughtThrow`], so a host can tell the
/// three apart at the door and not only in the emitted bytes.
pub(crate) const FAULT_UNCAUGHT_THROW: i32 = 2;

/// The script asked for something this engine does not have, and the answer
/// was only knowable at run time.
///
/// A **fourth** code for the same reason there is a third: a host that cannot
/// tell "your script threw" from "your script is broken" tells the author the
/// wrong thing -- and until this existed, "this engine has no such String
/// property" arrived as the bare `unreachable` that a genuine engine defect
/// executes. Those need different sentences: one is a boundary the author can
/// work around or ask to have moved, the other is a bug report.
///
/// It is a *class* rather than an instance, so a second producer joins it
/// rather than adding a fifth code. The distinction the word exists to carry
/// is what the **host** must say, and a host says the same thing about every
/// capability this engine lacks.
///
/// One producer today: `__obj_get`'s String arm. `"ab".length` is the only
/// property this engine answers, and the others keep trapping rather than
/// becoming `undefined` -- `"ab".toUpperCase` is a real function in ECMA-262,
/// so `undefined` would be a wrong answer wearing a right answer's clothes.
/// That decision is unchanged; what changes is that the trap now says which
/// kind of thing happened.
pub(crate) const FAULT_CAPABILITY: i32 = 3;

/// `mem[FAULT_WORD] = code`.
fn store_fault(code: i32, out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(FAULT_WORD));
    out.push(Ins::I32Const(code));
    out.push(Ins::I32Store(2, 0));
}

/// Emitted once, at the top of the entry point. Unused when this module is
/// included without `emit` -- see [`FAULT_NONE`].
#[allow(dead_code)]
pub(crate) fn clear_fault(out: &mut Vec<Ins>) {
    store_fault(FAULT_NONE, out);
}

/// Emitted where a throw becomes a trap: `__throw` in a module with no unwind
/// channel, and the entry point's epilogue where a throw ran out of handlers.
/// The write has to come first -- once the trap has happened there is no
/// guest instruction left to run, which is the same argument [`alloc`] makes
/// for writing [`FAULT_HEAP_EXHAUSTED`] before its own refusal.
pub(crate) fn record_uncaught_throw(out: &mut Vec<Ins>) {
    store_fault(FAULT_UNCAUGHT_THROW, out);
}

/// The second word of the fault area: where the thrown value's String record
/// is, when the uncaught throw threw a String, else 0.
///
/// A pointer and not a copy, because the record already lives in guest
/// memory and there are eight bytes below the literal pool -- exactly a fault
/// word and this. It is what turns "the script threw a value and nothing
/// caught it" into the value: every migrated gate script reports failure as
/// `throw "gate_id:reason"`, and a host that could not read the reason sent
/// the author to a manifest on disk to find out what happened.
///
/// Written only for a String. A thrown Number or object stays unreadable,
/// which is a narrowing worth stating rather than hiding: `String(e)` is the
/// spelling a script has if it wants the host to see a non-string.
pub(crate) const FAULT_THROWN: i32 = 4;

/// The script read a property of a String that this engine does not have,
/// and [`FAULT_THROWN`] holds the key's pooled String record so the host can
/// say which one.
///
/// `"ab".length` is the one String property `__obj_get` answers; every other
/// one has always trapped rather than become `undefined`, because
/// `"ab".slice` is a real function in ECMA-262 and `undefined` there would
/// be a wrong answer wearing a right answer's clothes. The trap was right
/// and illegible: it was a bare `unreachable` in a program that never said
/// `.length`, and a nameless [`FAULT_CAPABILITY`] in one that did. Every
/// script moving from rh met it as "guest trapped: unreachable executed" on
/// `slice`, `substr` and `substring`, and reported the three as three
/// different bugs. The key was in a local the whole time.
pub(crate) const FAULT_MISSING_STRING_METHOD: i32 = 5;

/// A host function was handed an argument of the wrong type at run time,
/// and [`FAULT_THROWN`] holds a pooled `"<host>#<n>"` (1-based argument
/// position) so the host can say which call and which argument.
///
/// `print(1)` is refused at compile time because a literal's type is known;
/// `print(s.length)` is not, because a receiver's type is a run-time fact,
/// and until now it reached `unbox_string`'s bare `unreachable` inside the
/// call-site's argument unwrapping. Every script author met it on their
/// first `print(n)`. A literal String argument skips the check altogether:
/// the compiler already knows.
pub(crate) const FAULT_HOST_ARGUMENT: i32 = 6;

/// Emitted where a host argument's tag test fails: the detail first, then
/// the code, then the caller's `unreachable`.
pub(crate) fn record_host_argument(detail: i32, out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(FAULT_THROWN));
    out.push(Ins::I32Const(detail));
    out.push(Ins::I32Store(2, 0));
    store_fault(FAULT_HOST_ARGUMENT, out);
}

/// `mem[FAULT_THROWN] = ptr` when the pair in (`tag`, `payload`) is a String,
/// else 0. `tag` and `payload` are the unwind channel's globals.
pub(crate) fn record_thrown_string(tag: u32, payload: u32, out: &mut Vec<Ins>) {
    // Two plain `if`s rather than an if/else with a result: this instruction
    // set has neither a typed block nor `else`. Clear first, then overwrite
    // when the tag says String -- a non-String throw leaves 0.
    out.push(Ins::I32Const(FAULT_THROWN));
    out.push(Ins::I32Const(0));
    out.push(Ins::I32Store(2, 0));
    out.push(Ins::GlobalGet(tag));
    out.push(Ins::I32Const(super::repr::TAG_STRING));
    out.push(Ins::I32Eq);
    out.push(Ins::If(BlockType::Empty));
    out.push(Ins::I32Const(FAULT_THROWN));
    out.push(Ins::GlobalGet(payload));
    out.push(Ins::I32WrapI64);
    out.push(Ins::I32Store(2, 0));
    out.push(Ins::End);
}

/// The emitted runtime functions, in index order. Position in [`SET`] is the
/// function's offset from [`Ctx::func_base`], so the list *is* the call table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rt {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Neg,
    TypeOf,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    ToNumber,
    Truthy,
    Len,
    Alloc,
    StrConcat,
    StrEq,
    // Objects, and the ToString every property key runs through.
    ToStr,
    ObjNew,
    ObjFind,
    ObjGet,
    ObjSet,
    // Functions. Appended, so every existing function keeps its index.
    FnNew,
}

/// Every runtime function, in the order they are defined in the module.
pub(crate) const SET: &[Rt] = &[
    Rt::Add,
    Rt::Sub,
    Rt::Mul,
    Rt::Div,
    Rt::Rem,
    Rt::Neg,
    Rt::TypeOf,
    Rt::Lt,
    Rt::Le,
    Rt::Gt,
    Rt::Ge,
    Rt::Eq,
    Rt::Ne,
    Rt::StrictEq,
    Rt::StrictNe,
    Rt::ToNumber,
    Rt::Truthy,
    Rt::Len,
    Rt::Alloc,
    Rt::StrConcat,
    Rt::StrEq,
    Rt::ToStr,
    Rt::ObjNew,
    Rt::ObjFind,
    Rt::ObjGet,
    Rt::ObjSet,
    Rt::FnNew,
];

impl Rt {
    /// The name the function is given in the module. Not exported -- these are
    /// the engine's own, and a guest that could call them by name could reach
    /// past the representation.
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Rt::Add => "__add",
            Rt::Sub => "__sub",
            Rt::Mul => "__mul",
            Rt::Div => "__div",
            Rt::Rem => "__rem",
            Rt::Neg => "__neg",
            Rt::TypeOf => "__typeof",
            Rt::Lt => "__lt",
            Rt::Le => "__le",
            Rt::Gt => "__gt",
            Rt::Ge => "__ge",
            Rt::Eq => "__eq",
            Rt::Ne => "__ne",
            Rt::StrictEq => "__strict_eq",
            Rt::StrictNe => "__strict_ne",
            Rt::ToNumber => "__to_number",
            Rt::Truthy => "__truthy",
            Rt::Len => "__len",
            Rt::Alloc => "__alloc",
            Rt::StrConcat => "__str_concat",
            Rt::StrEq => "__str_eq",
            Rt::ToStr => "__to_string",
            Rt::ObjNew => "__obj_new",
            Rt::ObjFind => "__obj_find",
            Rt::ObjGet => "__obj_get",
            Rt::ObjSet => "__obj_set",
            Rt::FnNew => "__fn_new",
        }
    }

    /// Offset of this function from [`Ctx::func_base`].
    pub(crate) fn offset(self) -> u32 {
        SET.iter()
            .position(|r| *r == self)
            .expect("SET lists every Rt") as u32
    }
}

/// One built runtime function, in the terms `emit` needs to hand it to `ir`:
/// a signature to intern, the locals beyond the parameters, and the body
/// without its terminating `end`.
#[derive(Debug, Clone)]
pub(crate) struct RtFunc {
    pub(crate) name: &'static str,
    pub(crate) params: Vec<ValType>,
    pub(crate) results: Vec<ValType>,
    pub(crate) locals: Vec<(u32, ValType)>,
    pub(crate) body: Vec<Ins>,
}

/// The five strings ECMA-262 13.5.3 can name over this engine's five types,
/// as guest addresses in the module's string pool.
///
/// They are pool records like any other literal, so `typeof x === "number"`
/// compares two records of the same shape -- and, because
/// [`StringPool::intern`] shares equal literals, usually the very same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TypeNames {
    pub(crate) number: i32,
    pub(crate) string: i32,
    pub(crate) boolean: i32,
    pub(crate) undefined: i32,
    /// 13.5.3 step 3: the name of the Null type is `"object"`.
    pub(crate) object: i32,
    /// 13.5.3 step 6: a callable answers `"function"` -- the one answer that
    /// is not the name of an ECMA-262 language type, because a function *is*
    /// an Object in the spec and is its own tag here.
    pub(crate) function: i32,
}

impl TypeNames {
    /// Intern all five, in the dispatch order [`super::repr`] documents.
    pub(crate) fn intern(pool: &mut StringPool) -> Self {
        Self {
            number: pool.intern("number"),
            string: pool.intern("string"),
            boolean: pool.intern("boolean"),
            undefined: pool.intern("undefined"),
            object: pool.intern("object"),
            function: pool.intern("function"),
        }
    }
}

/// The four fixed Strings `ToString` answers with for a Boolean, `null` and
/// `undefined` (ECMA-262 7.1.17), as guest addresses.
///
/// **Interned unconditionally, and that is a change from when they were
/// `ToPropertyKey`'s.** They used to be gated on the program writing a
/// *computed* member access, which a scan settles exactly. `__to_string` is
/// now also what `+` reaches when either operand is a String, and the gate
/// for *that* is "does this program contain an addition anywhere", counting
/// `+=` and every desugaring of it. An over-approximation costs the four
/// records; an under-approximation is a trap where an answer was due. Four
/// records are about 30 bytes against the 6.8 KiB of conversions every module
/// already carries, so the predicate is not worth its own risk.
///
/// [`TypeNames`] keeps its gate, because `typeof` is a syntactic construct and
/// the scan settles it with no approximation at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrimNames {
    pub(crate) yes: i32,
    pub(crate) no: i32,
    pub(crate) null: i32,
    pub(crate) undefined: i32,
}

impl PrimNames {
    pub(crate) fn intern(pool: &mut StringPool) -> Self {
        Self {
            yes: pool.intern("true"),
            no: pool.intern("false"),
            null: pool.intern("null"),
            undefined: pool.intern("undefined"),
        }
    }
}

/// Where the three ECMA-262 conversions [`super::convert`] emits live, as
/// function indices in the module being built.
///
/// Indices and not a `Cv`, so this module does not depend on that one. The
/// runtime needs *three functions*; which set they came from and where that
/// set was placed is the lowering's business. It is also what lets
/// `tests/repr_v1.rs` keep including this file without including the 23
/// functions of bignum beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Conversions {
    /// `(f64) -> i32`: 6.1.6.1.20 Number::toString, shortest round-tripping.
    pub(crate) num_to_string: u32,
    /// `(i32) -> f64`: 7.1.4.1 StringToNumber over the whole grammar.
    pub(crate) str_to_num: u32,
    /// `(i32, i32) -> i32`: 7.2.13's code-unit order, as -1, 0 or 1.
    pub(crate) str_cmp: u32,
}


pub(crate) struct Ctx {
    /// Function index of `__add`. Imports occupy the first indices, so this is
    /// the import count.
    pub(crate) func_base: u32,
    /// Index of the mutable `i32` global holding the bump pointer.
    pub(crate) heap_global: u32,
    /// Where `__typeof`'s five answers live, or `None` for a program that
    /// never writes `typeof`.
    ///
    /// A `Option` rather than five unconditional literals because the pool is
    /// the module's data segment: interning "number", "string", "boolean",
    /// "undefined" and "object" into every compiled module would cost 64 bytes
    /// of guest memory and shift every other literal's address, in a module
    /// that may have no `typeof` in it at all.
    pub(crate) type_names: Option<TypeNames>,
    /// Where `__to_string`'s four constant answers live. See [`PrimNames`]
    /// for why this one is not an `Option`.
    pub(crate) prim_names: PrimNames,
    /// Where the three conversions live. See [`Conversions`].
    pub(crate) conversions: Conversions,
    /// Where the string `"length"` lives, for a program that can read a
    /// property off a String -- or `None` for one that cannot.
    ///
    /// `Some` widens [`obj_get`] by one arm and interns four bytes; `None`
    /// leaves both exactly as they were. The predicate is: the program writes
    /// `.length` as a static key, **or** it writes any computed key at all,
    /// since a computed key can evaluate to `"length"` and the text cannot say
    /// it does not.
    ///
    /// Over-approximate in the computed case and exact otherwise, which is the
    /// most a gate on a *run-time* fact can be: unlike an ArrayLiteral, a
    /// String receiver is not something the source announces.
    pub(crate) string_length: Option<i32>,
    /// Whether any program text reads a static property other than
    /// `length`: the gate on `__obj_get`'s arm that names a missing String
    /// property. See `Scan::string_member`.
    pub(crate) string_member: bool,
    /// Whether any function in this program captures a binding of an
    /// enclosing one. Widens `__fn_new` by one parameter and the record it
    /// builds by one word; false leaves both exactly as they were.
    pub(crate) captures: bool,
    /// Whether this program can hold an Array -- the same gate
    /// [`super::array`]'s set is emitted under.
    ///
    /// This set is *unconditional*, so the two arms it controls -- `__typeof`'s
    /// and `__truthy`'s -- are in every module whether or not the flag exists.
    /// Measured: appending them cost **11 bytes to every program**, including
    /// `return 1;`, which is not the "nothing at all" the array gate promises
    /// and the JSON gate delivers. Eleven bytes is small and the promise is
    /// not: a gate that leaks is a gate nobody can quote.
    pub(crate) arrays: bool,
}

impl Ctx {
    /// The call every lowering site emits.
    pub(crate) fn call(&self, rt: Rt) -> Ins {
        Ins::Call(self.func_base + rt.offset())
    }

    fn num_to_string(&self) -> Ins {
        Ins::Call(self.conversions.num_to_string)
    }

    fn str_to_num(&self) -> Ins {
        Ins::Call(self.conversions.str_to_num)
    }

    fn str_cmp(&self) -> Ins {
        Ins::Call(self.conversions.str_cmp)
    }
}

/// Build every runtime function, in [`SET`] order.
pub(crate) fn build(ctx: &Ctx) -> Vec<RtFunc> {
    SET.iter().map(|rt| one(ctx, *rt)).collect()
}

fn one(ctx: &Ctx, rt: Rt) -> RtFunc {
    let (params, results, f) = match rt {
        Rt::Add => (values(2), values(1), add(ctx)),
        Rt::Sub => (values(2), values(1), arith(ctx, Ins::F64Sub)),
        Rt::Mul => (values(2), values(1), arith(ctx, Ins::F64Mul)),
        Rt::Div => (values(2), values(1), arith(ctx, Ins::F64Div)),
        Rt::Rem => (values(2), values(1), remainder(ctx)),
        Rt::Neg => (values(1), values(1), negate(ctx)),
        Rt::TypeOf => (values(1), values(1), type_of(ctx)),
        Rt::Lt => (values(2), values(1), relational(ctx, Ins::F64Lt)),
        Rt::Le => (values(2), values(1), relational(ctx, Ins::F64Le)),
        Rt::Gt => (values(2), values(1), relational(ctx, Ins::F64Gt)),
        Rt::Ge => (values(2), values(1), relational(ctx, Ins::F64Ge)),
        Rt::Eq => (values(2), values(1), loose_eq(ctx)),
        Rt::Ne => (values(2), values(1), negated(ctx, Rt::Eq)),
        Rt::StrictEq => (values(2), values(1), strict_eq(ctx)),
        Rt::StrictNe => (values(2), values(1), negated(ctx, Rt::StrictEq)),
        Rt::ToNumber => (values(1), vec![ValType::F64], to_number(ctx)),
        Rt::Truthy => (values(1), vec![ValType::I32], truthy(ctx)),
        Rt::Len => (values(1), values(1), length(ctx)),
        Rt::Alloc => (vec![ValType::I32], vec![ValType::I32], alloc(ctx)),
        Rt::StrConcat => (
            vec![ValType::I32, ValType::I32],
            vec![ValType::I32],
            str_concat(ctx),
        ),
        Rt::StrEq => (
            vec![ValType::I32, ValType::I32],
            vec![ValType::I32],
            str_eq(),
        ),
        Rt::ToStr => (values(1), vec![ValType::I32], to_string(ctx)),
        Rt::ObjNew => (vec![ValType::I32], vec![ValType::I32], obj_new(ctx)),
        Rt::ObjFind => (
            vec![ValType::I32, ValType::I32, ValType::I32],
            vec![ValType::I32],
            obj_find(ctx),
        ),
        Rt::ObjGet => (
            [values(1), vec![ValType::I32]].concat(),
            values(1),
            obj_get(ctx),
        ),
        Rt::ObjSet => (
            [values(1), vec![ValType::I32], values(1)].concat(),
            Vec::new(),
            obj_set(ctx),
        ),
        Rt::FnNew => (
            if ctx.captures {
                vec![ValType::I32, ValType::I32]
            } else {
                vec![ValType::I32]
            },
            vec![ValType::I32],
            fn_new(ctx),
        ),
    };
    RtFunc {
        name: rt.symbol(),
        params,
        results,
        locals: f.local_groups(),
        body: f.body,
    }
}

/// `n` JS values, flattened into wasm value types.
fn values(n: usize) -> Vec<ValType> {
    (0..n).flat_map(|_| repr::SLOTS).collect()
}

// ---- arithmetic ---------------------------------------------------------

/// `-` `*` `/`: ECMA-262 13.15.3 with no String operand possible -- every
/// operand goes through `ToNumber`, which is exactly what `__to_number` is.
///
/// No dispatch here at all, so the type count costs these three nothing: the
/// arms live once, inside `__to_number`.
fn arith(ctx: &Ctx, op: Ins) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let mut inner = Vec::new();
    to_number_of(ctx, 0, &mut inner);
    to_number_of(ctx, WIDTH, &mut inner);
    inner.push(op);
    box_number(&inner, &mut f.body);
    f
}

/// `%`: ECMA-262 13.15.3 over 6.1.6.1.6, Number::remainder.
///
/// # Why this is an algorithm and not three instructions
///
/// wasm has no `f64.rem`, and the obvious transcription of the spec's prose --
/// `n - trunc(n / d) * d` -- is **not** what the spec says. 6.1.6.1.6 defines
/// the result as `n - d * q`, where `q` is bounded by the magnitude of *the
/// true mathematical quotient*. `n / d` in binary64 is the true quotient
/// already rounded, so its truncation can be the wrong integer, and the
/// subtraction then removes the wrong multiple. `4611686014132420608 % 1000`
/// is 608; the transcription yields -512, which is not even a remainder.
///
/// So the reduction is done on exact terms instead. Scale `|d|` up by doubling
/// until one more doubling would pass `|n|`, then walk back down halving,
/// subtracting whenever the running value fits. Every step is exact: doubling
/// and halving a binary64 by two only moves the exponent, and each subtraction
/// happens with `m <= a < 2m`, where Sterbenz's lemma makes `a - m` exact. The
/// loop is bounded by the exponent range, so it terminates in at most about
/// 2100 turns of each half.
///
/// # The five special cases, in the order 6.1.6.1.6 lists them
///
/// NaN on either side, an infinite dividend, and a zero divisor are all NaN.
/// An infinite divisor and a zero dividend both give the dividend back --
/// and so does any dividend smaller in magnitude than the divisor, which is
/// why those three collapse into the single `|n| < |d|` test below once the
/// NaN and infinite-dividend cases are gone.
///
/// The sign is the *dividend's*, applied at the end with `f64.copysign`, which
/// is what makes this a remainder and not a modulo: `-5 % 3` is `-2`, and
/// `-6 % 3` is `-0`.
fn remainder(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let n = f.local(ValType::F64);
    let d = f.local(ValType::F64);
    let a = f.local(ValType::F64);
    let m = f.local(ValType::F64);

    to_number_of(ctx, 0, &mut f.body);
    f.body.push(Ins::LocalSet(n));
    to_number_of(ctx, WIDTH, &mut f.body);
    f.body.push(Ins::LocalSet(d));

    // NaN either side, |n| infinite, or d zero -- all NaN. `x != x` is the
    // NaN test, and `d == 0` catches both zeros.
    f.body.push(Ins::LocalGet(n));
    f.body.push(Ins::LocalGet(n));
    f.body.push(Ins::F64Ne);
    f.body.push(Ins::LocalGet(d));
    f.body.push(Ins::LocalGet(d));
    f.body.push(Ins::F64Ne);
    f.body.push(Ins::I32Or);
    f.body.push(Ins::LocalGet(n));
    f.body.push(Ins::F64Abs);
    f.body.push(Ins::F64Const(f64::INFINITY));
    f.body.push(Ins::F64Eq);
    f.body.push(Ins::I32Or);
    f.body.push(Ins::LocalGet(d));
    f.body.push(Ins::F64Const(0.0));
    f.body.push(Ins::F64Eq);
    f.body.push(Ins::I32Or);
    f.body.push(Ins::If(BlockType::Empty));
    box_number(&[Ins::F64Const(f64::NAN)], &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // Nothing to reduce: a zero dividend, an infinite divisor, or a dividend
    // already smaller than the divisor. The answer is the dividend itself,
    // sign and all.
    f.body.push(Ins::LocalGet(n));
    f.body.push(Ins::F64Abs);
    f.body.push(Ins::LocalGet(d));
    f.body.push(Ins::F64Abs);
    f.body.push(Ins::F64Lt);
    f.body.push(Ins::If(BlockType::Empty));
    box_number(&[Ins::LocalGet(n)], &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // From here both are finite, `d` is not zero, and `|n| >= |d|`. Work on
    // magnitudes; `d` holds `|d|` for the rest of the function.
    f.body.push(Ins::LocalGet(n));
    f.body.push(Ins::F64Abs);
    f.body.push(Ins::LocalSet(a));
    f.body.push(Ins::LocalGet(d));
    f.body.push(Ins::F64Abs);
    f.body.push(Ins::LocalTee(d));
    f.body.push(Ins::LocalSet(m));

    // Scale up: the largest `|d| * 2^k` that does not pass `a`. `m + m`
    // overflowing to infinity simply fails the test and ends the loop.
    f.body.push(Ins::Block(BlockType::Empty));
    f.body.push(Ins::Loop(BlockType::Empty));
    f.body.push(Ins::LocalGet(m));
    f.body.push(Ins::LocalGet(m));
    f.body.push(Ins::F64Add);
    f.body.push(Ins::LocalGet(a));
    f.body.push(Ins::F64Le);
    f.body.push(Ins::I32Eqz);
    f.body.push(Ins::BrIf(1));
    f.body.push(Ins::LocalGet(m));
    f.body.push(Ins::LocalGet(m));
    f.body.push(Ins::F64Add);
    f.body.push(Ins::LocalSet(m));
    f.body.push(Ins::Br(0));
    f.body.push(Ins::End);
    f.body.push(Ins::End);

    // Walk back down. The invariant entering each turn is `a < 2m`, so every
    // subtraction that happens is exact, and after the turn at `m == |d|` the
    // remaining `a` is the answer.
    f.body.push(Ins::Block(BlockType::Empty));
    f.body.push(Ins::Loop(BlockType::Empty));
    f.body.push(Ins::LocalGet(a));
    f.body.push(Ins::LocalGet(m));
    f.body.push(Ins::F64Ge);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::LocalGet(a));
    f.body.push(Ins::LocalGet(m));
    f.body.push(Ins::F64Sub);
    f.body.push(Ins::LocalSet(a));
    f.body.push(Ins::End);
    f.body.push(Ins::LocalGet(m));
    f.body.push(Ins::LocalGet(d));
    f.body.push(Ins::F64Eq);
    f.body.push(Ins::BrIf(1));
    f.body.push(Ins::LocalGet(m));
    f.body.push(Ins::F64Const(0.5));
    f.body.push(Ins::F64Mul);
    f.body.push(Ins::LocalSet(m));
    f.body.push(Ins::Br(0));
    f.body.push(Ins::End);
    f.body.push(Ins::End);

    box_number(
        &[Ins::LocalGet(a), Ins::LocalGet(n), Ins::F64Copysign],
        &mut f.body,
    );
    f
}

/// Unary `-`: ECMA-262 13.5.5. `f64.neg` flips the sign bit, so `-(+0)` is
/// `-0` and `-NaN` keeps its payload, both of which the spec requires.
fn negate(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let mut inner = Vec::new();
    to_number_of(ctx, 0, &mut inner);
    inner.push(Ins::F64Neg);
    box_number(&inner, &mut f.body);
    f
}

/// `typeof`: ECMA-262 13.5.3, one string per language type.
///
/// Five arms and no default that guesses: every tag this engine defines is
/// listed, so reaching the end means the pair was not built by this engine.
/// The order is `repr`'s documented one -- Number, then String, then the rest
/// -- so adding a type appends an arm and costs the existing ones nothing.
///
/// `Ctx::type_names` is `None` for a program with no `typeof` in it, and then
/// nothing in the module calls this function. Its body is the trap that says
/// so, rather than five literals no script can reach.
fn type_of(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let Some(names) = ctx.type_names else {
        f.body.push(Ins::Unreachable);
        return f;
    };
    for (test, at) in [
        (is_number as fn(u32, &mut Vec<Ins>), names.number),
        (is_string, names.string),
        (is_bool, names.boolean),
        (is_undefined, names.undefined),
        // 13.5.3 step 3 gives Null the name "object", and step 8 gives an
        // ordinary Object the same one. Two arms, one string.
        (is_null, names.object),
        (is_object, names.object),
        // Appended last, so no type that existed before functions did pays a
        // test for them -- the rule `repr`'s *Dispatch order* states.
        (is_function, names.function),
    ] {
        test(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        const_string(at, &mut f.body);
        f.body.push(Ins::Return);
        f.body.push(Ins::End);
    }
    // 13.5.3 step 8 again: an Array is an ordinary Object as far as `typeof`
    // is concerned, so this is a third arm answering the same string.
    // `Array.isArray` is what distinguishes them in ECMA-262 and this engine
    // does not have it -- worth knowing before reading `typeof a === "object"`
    // as "not an array".
    //
    // Appended last, under the dispatch-order rule, *and* emitted only for a
    // program that can hold one. It is outside the loop for that second
    // reason: this set is unconditional, so an arm added here is an arm in
    // every module, and the eleven bytes it costs `return 1;` are eleven more
    // than the array gate promises.
    if ctx.arrays {
        is_array(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        const_string(names.object, &mut f.body);
        f.body.push(Ins::Return);
        f.body.push(Ins::End);
    }
    f.body.push(Ins::Unreachable);
    f
}

/// `+`: ECMA-262 13.15.3, ApplyStringOrNumericBinaryOperator.
///
/// `ToPrimitive` is the identity on every primitive, so the spec reduces to:
/// if either side is a String, concatenate the `ToString`s; otherwise add the
/// `ToNumber`s. An Object operand is the one case where `ToPrimitive` is not
/// the identity, and it reaches `__to_number`, whose Object arm is the
/// `unreachable` that says so.
///
/// Number is tested first. That is the documented dispatch order (see
/// [`super::repr`]), and the experiment measured what the other order costs:
/// String-first made every numeric addition pay the String test, for 2 619
/// extra steps across a corpus with no strings in it.
fn add(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);

    is_number(0, &mut f.body);
    is_number(WIDTH, &mut f.body);
    f.body.push(Ins::I32And);
    f.body.push(Ins::If(BlockType::Empty));
    let mut inner = Vec::new();
    unbox_number(0, &mut inner);
    unbox_number(WIDTH, &mut inner);
    inner.push(Ins::F64Add);
    box_number(&inner, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // 13.15.3 step 1.d: *either* side a String makes this the string branch,
    // and both sides then run ToString. Not "both sides already Strings" --
    // that was this arm while `__to_string` could only answer for one type,
    // and it is the narrowing the README used to describe.
    is_string(0, &mut f.body);
    is_string(WIDTH, &mut f.body);
    f.body.push(Ins::I32Or);
    f.body.push(Ins::If(BlockType::Empty));
    let mut inner = Vec::new();
    load_local(0, &mut inner);
    inner.push(ctx.call(Rt::ToStr));
    load_local(WIDTH, &mut inner);
    inner.push(ctx.call(Rt::ToStr));
    inner.push(ctx.call(Rt::StrConcat));
    box_string(&inner, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // Neither side is a String, so this is the numeric branch of the spec.
    let mut inner = Vec::new();
    to_number_of(ctx, 0, &mut inner);
    to_number_of(ctx, WIDTH, &mut inner);
    inner.push(Ins::F64Add);
    box_number(&inner, &mut f.body);
    f
}

/// `<` `<=` `>` `>=`: ECMA-262 13.10 over 7.2.13, IsLessThan.
///
/// Two Numbers take the direct path; two Strings would compare by code unit,
/// which is not implemented; everything else goes through `ToNumber`. The
/// `f64` comparisons already return false for NaN on both sides, which is what
/// 7.2.13's *undefined* result becomes at 13.10.
fn relational(ctx: &Ctx, op: Ins) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);

    is_number(0, &mut f.body);
    is_number(WIDTH, &mut f.body);
    f.body.push(Ins::I32And);
    f.body.push(Ins::If(BlockType::Empty));
    let mut inner = Vec::new();
    unbox_number(0, &mut inner);
    unbox_number(WIDTH, &mut inner);
    inner.push(op);
    box_bool(&inner, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // 7.2.13 step 3: the code-unit comparison happens only when *both* sides
    // are Strings. A mixed pair falls through to step 4 and runs ToNumeric on
    // each, which is why this test is an `and` where `__add`'s is an `or`.
    is_string(0, &mut f.body);
    is_string(WIDTH, &mut f.body);
    f.body.push(Ins::I32And);
    f.body.push(Ins::If(BlockType::Empty));
    let mut inner = Vec::new();
    unbox_string(0, &mut inner);
    unbox_string(WIDTH, &mut inner);
    inner.push(ctx.str_cmp());
    inner.extend_from_slice(sign_test(op));
    box_bool(&inner, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    let mut inner = Vec::new();
    to_number_of(ctx, 0, &mut inner);
    to_number_of(ctx, WIDTH, &mut inner);
    inner.push(op);
    box_bool(&inner, &mut f.body);
    f
}

/// Turn `__str_cmp`'s answer into the answer the operator wants. The stack
/// already holds it when this run starts.
///
/// `__str_cmp` answers with exactly -1, 0 or 1 -- its own doc comment says so
/// and every `return` in it pushes one of the three -- so each of the four
/// tests is one equality against one constant rather than a signed
/// comparison. That is not a shortcut around a missing opcode; it is what a
/// three-valued answer makes true. `repr`'s instruction set does happen to
/// lack `i32.gt_s` and `i32.le_s`, which is how the narrowness got noticed.
fn sign_test(op: Ins) -> &'static [Ins] {
    match op {
        // c < 0, and over {-1, 0, 1} that is c == -1.
        Ins::F64Lt => &[Ins::I32Const(-1), Ins::I32Eq],
        // c > 0
        Ins::F64Gt => &[Ins::I32Const(1), Ins::I32Eq],
        // c <= 0, that is c != 1
        Ins::F64Le => &[Ins::I32Const(1), Ins::I32Ne],
        // c >= 0, that is c != -1
        Ins::F64Ge => &[Ins::I32Const(-1), Ins::I32Ne],
        _ => unreachable!("relational is built with one of the four f64 comparisons"),
    }
}

// ---- equality -----------------------------------------------------------

/// `===`: ECMA-262 7.2.15, IsStrictlyEqual. Complete over this engine's five
/// types -- there is nothing to defer, because strict equality never coerces.
///
/// Three arms, not five. Different tags means different ECMA-262 language
/// types, because there is one tag per type. Within a type, Number needs
/// `f64.eq` (NaN is unequal to itself and `+0` equals `-0`, neither of which a
/// bit compare gets right) and String needs a content compare; Boolean,
/// Undefined and Null all fall to a single `i64.eq` on the payload, which the
/// payload-0 invariant in [`super::repr`] is what makes sound.
fn strict_eq(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);

    same_type(0, WIDTH, &mut f.body);
    f.body.push(Ins::I32Eqz);
    f.body.push(Ins::If(BlockType::Empty));
    const_bool(false, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_number(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    let mut inner = Vec::new();
    unbox_number(0, &mut inner);
    unbox_number(WIDTH, &mut inner);
    inner.push(Ins::F64Eq);
    box_bool(&inner, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_string(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    let mut inner = Vec::new();
    unbox_string(0, &mut inner);
    unbox_string(WIDTH, &mut inner);
    inner.push(ctx.call(Rt::StrEq));
    box_bool(&inner, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    let inner = vec![Ins::LocalGet(1), Ins::LocalGet(WIDTH + 1), Ins::I64Eq];
    box_bool(&inner, &mut f.body);
    f
}

/// `==`: ECMA-262 7.2.14, IsLooselyEqual.
///
/// Same type defers to `===` (step 1). `null == undefined` is true and nothing
/// else is loosely equal to either (steps 2, 3, and the absence of any other
/// rule naming them). A String opposite a Number or a Boolean needs
/// `StringToNumber`, which is not implemented. What is left is Number opposite
/// Boolean, in either order, and `ToNumber` settles it (steps 8 and 9).
fn loose_eq(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);

    same_type(0, WIDTH, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    load_local(0, &mut f.body);
    load_local(WIDTH, &mut f.body);
    f.body.push(ctx.call(Rt::StrictEq));
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_nullish(0, &mut f.body);
    is_nullish(WIDTH, &mut f.body);
    f.body.push(Ins::I32Or);
    f.body.push(Ins::If(BlockType::Empty));
    let mut inner = Vec::new();
    is_nullish(0, &mut inner);
    is_nullish(WIDTH, &mut inner);
    inner.push(Ins::I32And);
    box_bool(&inner, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // Steps 4 to 9: a String opposite a Number or a Boolean is settled by
    // ToNumber on each, and `__to_number` now has the String arm 7.1.4.1
    // needs. No arm of its own: the two-sided ToNumber below *is* those steps.
    let mut inner = Vec::new();
    to_number_of(ctx, 0, &mut inner);
    to_number_of(ctx, WIDTH, &mut inner);
    inner.push(Ins::F64Eq);
    box_bool(&inner, &mut f.body);
    f
}

/// `!=` and `!==`: the negation of the corresponding equality, per 13.11.1.
/// Written as a call rather than a copy so the two can never drift apart.
fn negated(ctx: &Ctx, of: Rt) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let tmp = f.value_local();
    load_local(0, &mut f.body);
    load_local(WIDTH, &mut f.body);
    f.body.push(ctx.call(of));
    store_local(tmp, &mut f.body);
    let mut inner = Vec::new();
    unbox_bool(tmp, &mut inner);
    inner.push(Ins::I32Eqz);
    box_bool(&inner, &mut f.body);
    f
}

// ---- conversions --------------------------------------------------------

/// ECMA-262 7.1.4, ToNumber. One arm per type, so no other function needs a
/// numeric-coercion arm of its own -- which is why the type count costs
/// `__sub`, `__mul`, `__div` and `__neg` nothing.
fn to_number(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);

    is_number(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_number(0, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // 7.1.4 step 3 over 7.1.4.1 StringToNumber, which is the whole
    // `StringNumericLiteral` grammar and lives in [`super::convert`].
    is_string(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_string(0, &mut f.body);
    f.body.push(ctx.str_to_num());
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_bool(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_bool(0, &mut f.body);
    f.body.push(Ins::F64ConvertI32S);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_null(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::F64Const(0.0));
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_undefined(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::F64Const(f64::NAN));
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // 7.1.4 step 9: ToNumber of an Object is ToNumber of its ToPrimitive, and
    // 7.1.1 reaches `valueOf`/`toString` through a prototype this engine does
    // not have. Written as its own arm rather than left to the fallthrough, so
    // that the fallthrough keeps meaning "not built by this engine".
    is_object(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::Unreachable);
    f.body.push(Ins::End);

    // A function is an Object for the same step, and reaches the same missing
    // algorithm -- `Function.prototype.toString` is a prototype method and
    // there is no prototype. Appended last, and written out for the same
    // reason the Object arm is.
    is_function(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::Unreachable);
    f.body.push(Ins::End);

    // Every tag is accounted for above, so reaching here means the pair was
    // not built by this engine.
    f.body.push(Ins::Unreachable);
    f
}

/// ECMA-262 7.1.2, ToBoolean. Complete over the five types.
///
/// `+0`, `-0` and `NaN` are the falsy Numbers, which is why the Number arm
/// needs the value three times and therefore a scratch `f64` local.
fn truthy(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let scratch = f.local(ValType::F64);

    is_number(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_number(0, &mut f.body);
    f.body.push(Ins::LocalTee(scratch));
    f.body.push(Ins::F64Const(0.0));
    f.body.push(Ins::F64Ne);
    f.body.push(Ins::LocalGet(scratch));
    f.body.push(Ins::LocalGet(scratch));
    f.body.push(Ins::F64Eq);
    f.body.push(Ins::I32And);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_string(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_string(0, &mut f.body);
    f.body.push(Ins::I32Load(2, 0));
    f.body.push(Ins::I32Const(0));
    f.body.push(Ins::I32Ne);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_bool(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_bool(0, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_nullish(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::I32Const(0));
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // 7.1.2 step 8: an Object is always true. Not "an object with properties"
    // -- `{}` is truthy, which is the case an emptiness shortcut gets wrong.
    is_object(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::I32Const(1));
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // The same step 8: a function is an Object, so it is true -- including a
    // function with no parameters and an empty body. Appended last.
    is_function(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::I32Const(1));
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // Step 8 a third time, and the one that surprises people: `[]` is truthy.
    // An empty array is an Object, and 7.1.2 never looks inside one. Appended
    // last, and emitted only under the array gate, for the reason
    // [`type_of`]'s arm gives.
    if ctx.arrays {
        is_array(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        f.body.push(Ins::I32Const(1));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);
    }

    f.body.push(Ins::Unreachable);
    f
}

/// `.length` of a String, as a Number. Traps on anything else, from
/// `unbox_string`.
///
/// # This counts UTF-16 code units, not bytes
///
/// ECMA-262 6.1.4 makes a String a sequence of **UTF-16 code units**, and
/// 22.1.3.2 makes `length` their count. This engine stores UTF-8, so the byte
/// count in the record header is a different number the moment any character
/// leaves ASCII: `"café"` is 5 bytes and 4 code units, and an emoji is 4 bytes
/// and 2 code units.
///
/// Returning the byte count would be a wrong answer wearing a right answer's
/// clothes -- it agrees with the spec on every ASCII string, which is most of
/// them, and disagrees silently on the rest. So the count is computed:
///
/// * a byte that is not a continuation byte (`0b10xxxxxx`) starts a character,
///   and every character is at least one code unit;
/// * a byte at or above `0xf0` starts a four-byte sequence, whose code point
///   is above U+FFFF and is therefore a **surrogate pair** -- one more unit.
///
/// The string is valid UTF-8 by construction (it is either a literal the
/// compiler interned or something `__str_concat` built out of two such), so
/// this needs no validation pass, only a count.
fn length(ctx: &Ctx) -> FnBuild {
    // `__len` sits in the *unconditional* set, so its index is fixed and it is
    // emitted whether or not anything calls it. Only [`obj_get`]'s gated arm
    // can, so with the gate off this body is unreachable and the counter above
    // would be ninety-odd bytes of dead weight in every module -- which is the
    // shape `plan/design-array-milestone.md` §1.1 already caught once, when
    // two arms went into the unconditional runtime and cost every program 11
    // bytes. The function still exists at its index; its body does not.
    if ctx.string_length.is_none() {
        let mut f = FnBuild::new(WIDTH);
        f.body.push(Ins::Unreachable);
        return f;
    }
    let mut f = FnBuild::new(WIDTH);
    let p = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let bytes = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let byte = f.local(ValType::I32);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(p));

    let b = &mut f.body;
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(bytes));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(bytes));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));

    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    // Offset 4: the byte after the length header.
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::LocalSet(byte));

    // Not a continuation byte: `(byte & 0xc0) != 0x80`.
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(0xc0));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(n));
    // A four-byte sequence is a surrogate pair: one unit more.
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(0xf0));
    b.push(Ins::I32GeU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(n));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    let inner = vec![
        Ins::LocalGet(n),
        Ins::F64ConvertI32S,
    ];
    let mut out = Vec::new();
    box_number(&inner, &mut out);
    f.body.extend(out);
    f
}

/// Push the `f64` value of the JS value held at local `base`.
fn to_number_of(ctx: &Ctx, base: u32, out: &mut Vec<Ins>) {
    load_local(base, out);
    out.push(ctx.call(Rt::ToNumber));
}

// ---- heap ---------------------------------------------------------------

/// Bump allocation, 4-byte aligned, with no free and no collector. Grows
/// linear memory rather than trapping at the first page boundary; the host's
/// [`tinyvm::Limits`] is what actually bounds it, which is where the bound
/// belongs.
///
/// When that bound is reached the allocator still has to fail, and it records
/// [`FAULT_HEAP_EXHAUSTED`] in [`FAULT_WORD`] before it does, so the host can
/// tell "out of budget" from "broken script" without matching on a trap
/// message that is identical for both.
///
/// # The postcondition, checked rather than assumed
///
/// [`FAULT_WORD`]'s doc rests on one claim: *the bump pointer is never below
/// [`DATA_ORIGIN`]*, so the fault word is a word no allocation can ever hand
/// out. Rounding with `(size + 3) & -4` does not preserve that on its own. The
/// rounded size is negative for every `size <= -1`, and it is negative again
/// for a `size` within three of `i32::MAX`, where `size + 3` overflows. Either
/// one moves the bump pointer *backwards*, and enough of them walk it past
/// [`DATA_ORIGIN`] to zero, where the next record written lands on top of the
/// fault word -- and the guest then answers "out of budget" for a script whose
/// only problem was a type error.
///
/// So the claim is a check, not a comment: if the bump pointer did not move
/// forward, this call is refused. It is stated as `new <u old` rather than
/// `size >= 0` deliberately, because that is the postcondition itself -- it
/// covers the negative size, the overflowing size and any future arithmetic
/// here, where a sign test would only cover the first.
///
/// Nothing is written to [`FAULT_WORD`] on this path. A size the allocator
/// cannot represent is not a budget the host can raise, and claiming
/// [`FAULT_HEAP_EXHAUSTED`] for it would be the same misclassification the
/// fault word exists to prevent, only pointing the other way.
fn alloc(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let p = f.local(ValType::I32);
    let g = ctx.heap_global;
    let b = &mut f.body;
    b.push(Ins::GlobalGet(g));
    b.push(Ins::LocalSet(p));
    b.push(Ins::GlobalGet(g));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Const(3));
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(-4));
    b.push(Ins::I32And);
    b.push(Ins::I32Add);
    b.push(Ins::GlobalSet(g));
    // The postcondition, checked: the bump pointer only ever moves forward.
    b.push(Ins::GlobalGet(g));
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::Unreachable);
    b.push(Ins::End);
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::MemorySize);
    b.push(Ins::I32Const(16));
    b.push(Ins::I32Shl);
    b.push(Ins::GlobalGet(g));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::I32Const(1));
    b.push(Ins::MemoryGrow);
    b.push(Ins::I32Const(-1));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    // The refusal is about to become an ordinary `unreachable`, which says
    // nothing. Say it first -- see [`FAULT_WORD`].
    store_fault(FAULT_HEAP_EXHAUSTED, b);
    b.push(Ins::Unreachable);
    b.push(Ins::End);
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::LocalGet(p));
    f
}

/// `[len: i32][bytes]`, UTF-8, no interning.
fn str_concat(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2);
    let la = f.local(ValType::I32);
    let lb = f.local(ValType::I32);
    let p = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let b = &mut f.body;
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(2, 0));
    b.push(Ins::LocalSet(la));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32Load(2, 0));
    b.push(Ins::LocalSet(lb));
    b.push(Ins::LocalGet(la));
    b.push(Ins::LocalGet(lb));
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.call(Rt::Alloc));
    b.push(Ins::LocalSet(p));
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(la));
    b.push(Ins::LocalGet(lb));
    b.push(Ins::I32Add);
    b.push(Ins::I32Store(2, 0));
    copy_loop(b, 0, la, p, None, i);
    copy_loop(b, 1, lb, p, Some(la), i);
    b.push(Ins::LocalGet(p));
    f
}

/// `dst[shift + k] = src[k]` for `k < len`, one byte at a time. A byte loop
/// rather than `memory.copy`: bulk memory is post-MVP, and this compiler's
/// output has to clear tinyvm's load gate on MVP terms.
fn copy_loop(b: &mut Vec<Ins>, src: u32, len: u32, dst: u32, shift: Option<u32>, i: u32) {
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(i));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(len));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(dst));
    if let Some(shift) = shift {
        b.push(Ins::LocalGet(shift));
        b.push(Ins::I32Add);
    }
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(src));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    // Offset 4 on both sides: the byte after the length header.
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Store8(0, 4));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
}

/// Byte equality. Not pointer equality: there is no interning, so two equal
/// strings routinely live at two addresses.
fn str_eq() -> FnBuild {
    let mut f = FnBuild::new(2);
    let la = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let b = &mut f.body;
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(2, 0));
    b.push(Ins::LocalSet(la));
    b.push(Ins::LocalGet(la));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32Load(2, 0));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(0));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(i));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(la));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::LocalGet(1));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(0));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::I32Const(1));
    f
}

// ---- objects ------------------------------------------------------------

/// The object record: `[len: i32][cap: i32][entries: i32]`, and an entry
/// vector of `cap` entries of [`ENTRY_BYTES`] each.
///
/// # Why a flat key/value vector, and when it stops being right
///
/// A real engine gives an object a *shape* -- a hidden class shared by every
/// object built the same way -- so that `o.a` becomes a load at a constant
/// offset once the shape is known, and so that a hundred objects of the same
/// shape store their keys once between them. That is the right design and it
/// is not the right design *here*, for two measured reasons about the scripts
/// this milestone exists to compile.
///
/// The binding library this targets (`agenterm/scripts/qjs/lib/fleet.js`)
/// builds exactly two kinds of object: **namespace tables**, of which there
/// are twelve, each built once and never again, the largest holding ten
/// properties; and **parameter objects** of one to three fields, built fresh
/// at every call and thrown away. Neither benefits from a shape table:
///
/// * The namespace tables are each of a *different* shape, and each exists in
///   one copy. A shape table would hold twelve entries used once apiece -- the
///   sharing that pays for a hidden class never happens.
/// * The parameter objects are built by literal, so their offsets are already
///   known to the compiler at the only site that writes them, and read once by
///   `JSON.stringify` -- a walk that visits every property anyway, which is
///   the access pattern a linear vector is already optimal for.
///
/// The lookup cost that buys is a linear scan of at most ten `__str_eq` calls,
/// and in the overwhelming case a scan of one to three. The cost a shape table
/// would add is a transition table, a shape identity, and a second allocation
/// per object -- machinery whose whole payoff is a case this workload does not
/// contain.
///
/// **Where the choice stops being right**, stated so the next milestone does
/// not have to rediscover it:
///
/// 1. **Many objects of one shape.** The moment a script builds objects in a
///    loop -- rows out of a `ui.snapshot`, one object per tab -- the keys are
///    stored once per object and the duplication is the whole heap. That is
///    the shape table's case, and it arrives with arrays and `JSON.parse`, not
///    with this milestone.
/// 2. **Objects past roughly sixteen properties.** A linear scan of ten
///    `__str_eq` calls is cheap because each is a length compare that usually
///    fails on the first byte; at sixty keys it is not, and a hash of the key
///    string belongs in the record.
/// 3. **A key looked up in a loop.** `while (...) { o[k] = o[k] + 1; }` pays
///    the scan twice per turn. An inline cache -- remember the entry index
///    this site found last time -- is the cure, and it needs a stable entry
///    index, which this layout already has because entries never move within a
///    record. Growth reallocates the vector but keeps every index.
///
/// None of the three is a reason to build the shape table now, and all three
/// are reasons this comment exists.
pub(crate) const OBJ_HEADER: i32 = 12;
pub(crate) const OBJ_LEN: u32 = 0;
pub(crate) const OBJ_CAP: u32 = 4;
pub(crate) const OBJ_ENTRIES: u32 = 8;

/// The `[len: i32][utf8 bytes]` record's header, in bytes. A reader of a
/// string wants the bytes, which start after it.
pub(crate) const STRING_HEADER: i32 = 4;

/// One property: `[key: i32][tag: i32][payload: i64]`.
///
/// The key is a pointer to an ordinary string record, so a key and a String
/// value are the same kind of thing and `__str_eq` compares them. The value is
/// the V1 pair stored whole -- tag beside payload -- which is what makes a
/// read a load of two words and not a re-boxing.
pub(crate) const ENTRY_BYTES: i32 = 16;
pub(crate) const ENTRY_KEY: u32 = 0;
pub(crate) const ENTRY_TAG: u32 = 4;
pub(crate) const ENTRY_PAYLOAD: u32 = 8;

/// The alignment *exponent* every access into a record declares: 2, meaning
/// four bytes. `__alloc` aligns to four, so the eight-byte alignment an
/// `i64.load` would naturally claim is not something this module may promise.
/// Below-natural is legal wasm and is a hint only.
pub(crate) const ALIGN_WORD: u32 = 2;

/// The entry vector a record allocates the first time it has to grow, and the
/// factor it grows by afterwards.
///
/// A literal is built at its own exact size instead (`__obj_new` takes the
/// count), so this number is only ever reached by an object filled with
/// assignments -- the namespace-table pattern, which reaches four properties
/// in nine of `fleet.js`'s twelve tables and ten in the largest. Four then
/// eight then sixteen reaches every one of them in at most two reallocations.
const FIRST_CAP: i32 = 4;
const GROWTH: i32 = 2;

/// `__obj_new(cap) -> i32`: an empty record with room for `cap` properties.
///
/// The capacity is the caller's because the caller usually knows it exactly:
/// an object literal has its properties counted at compile time and never
/// reallocates. `__obj_new(0)` allocates no entry vector at all, which is what
/// `{}` wants -- and `{}` is how every namespace table in `fleet.js` starts.
fn obj_new(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let p = f.local(ValType::I32);
    let b = &mut f.body;
    b.push(Ins::I32Const(OBJ_HEADER));
    b.push(ctx.call(Rt::Alloc));
    b.push(Ins::LocalSet(p));
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32Const(0));
    b.push(Ins::I32Store(ALIGN_WORD, OBJ_LEN));
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Store(ALIGN_WORD, OBJ_CAP));
    // Written before the test rather than in an `else`: `repr`'s `BlockType`
    // has only `Empty` and its instruction set has no `else`, and a zero
    // pointer is the honest value for "no entry vector" anyway.
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32Const(0));
    b.push(Ins::I32Store(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalGet(0));
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Const(ENTRY_BYTES));
    b.push(Ins::I32Mul);
    b.push(ctx.call(Rt::Alloc));
    b.push(Ins::I32Store(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::End);
    b.push(Ins::LocalGet(p));
    f
}

/// `__obj_find(entries, len, key) -> i32`: the index of `key`, or `-1`.
///
/// Byte equality through `__str_eq`, not pointer equality: the string pool
/// interns equal *literals*, but a key computed at run time -- `o["a" + "b"]`,
/// or the digits `__num_to_string` just allocated -- is a fresh record. Comparing
/// pointers would make `o[1]` and `o["1"]` two properties, which is the exact
/// thing ECMA-262 7.1.19 says they are not.
fn obj_find(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(3);
    let i = f.local(ValType::I32);
    let b = &mut f.body;
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(i));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    entry_at(b, 0, i);
    b.push(Ins::I32Load(ALIGN_WORD, ENTRY_KEY));
    b.push(Ins::LocalGet(2));
    b.push(ctx.call(Rt::StrEq));
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::I32Const(-1));
    f
}

/// Push `entries + index * ENTRY_BYTES`, both read from locals.
fn entry_at(b: &mut Vec<Ins>, entries: u32, index: u32) {
    b.push(Ins::LocalGet(entries));
    b.push(Ins::LocalGet(index));
    b.push(Ins::I32Const(ENTRY_BYTES));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
}

/// `__obj_get(value, key) -> value`: ECMA-262 10.1.8.1 OrdinaryGet, over an
/// object with no prototype.
///
/// # Two answers that are not the same shape of "no"
///
/// A **property that is not there** is `undefined`, not a fault. That is
/// 10.1.8.1 step 2 with a null prototype, and it is the single most common
/// thing a real script does -- `if (o.note)`. A trap here would make the
/// engine unusable for the scripts it exists to run.
///
/// A **receiver that is not an Object** is a fault. `undefined.a` and `null.a`
/// are TypeErrors in ECMA-262 and this is the closest thing to a throw the
/// engine has. The other three primitives are the interesting case: 13.3.2.1
/// wraps them with ToObject, and `"abc".length` is `3` only because
/// `String.prototype` exists. It does not exist here, so answering `undefined`
/// would be the *right answer reached by the wrong route* -- and silently
/// wrong the moment the property is one a prototype really has. The receiver
/// test is `unbox_object`, so all four cases are one trap.
///
/// # Dispatch order
///
/// The receiver is tested for Object **first**, which departs from `repr`'s
/// documented Number-then-String order. It is the departure that order's own
/// rule asks for: in every non-erroneous program the receiver of a property
/// access *is* an Object, so testing anything before it would put a test in
/// front of the only path that ever succeeds.
fn obj_get(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH + 1);
    let key = WIDTH;
    let o = f.local(ValType::I32);
    let idx = f.local(ValType::I32);
    let e = f.local(ValType::I32);

    // A String receiver, before the Object test, because `unbox_object` traps
    // on one. `"ab".length` is the only property this engine can answer, and
    // every other one keeps trapping rather than becoming `undefined` --
    // `"ab".toUpperCase` is a real function in ECMA-262, so `undefined` there
    // would be a wrong answer wearing a right answer's clothes. That is the
    // opposite of the choice `prop_get` makes for an array index, and for a
    // reason: an absent index really is absent, an absent String method is
    // one this engine does not have yet.
    // Two shapes, chosen by what the program can reach. A program whose
    // only static member read is `.length` keeps the arm it had: answer
    // `length`, else a nameless capability fault. One that reads any other
    // static property can reach this arm with a key `__obj_get` cannot
    // answer, and for it the arm writes the key's record where the host can
    // read it (see [`FAULT_MISSING_STRING_METHOD`]) -- 23 bytes, paid only
    // by programs that can use them. `Rt::Len` is only called when its gate
    // built it.
    if ctx.string_member || ctx.string_length.is_some() {
        let mut arm = Vec::new();
        is_string(0, &mut arm);
        arm.push(Ins::If(BlockType::Empty));
        if let Some(length) = ctx.string_length {
            arm.push(Ins::LocalGet(key));
            arm.push(Ins::I32Const(length));
            arm.push(ctx.call(Rt::StrEq));
            arm.push(Ins::If(BlockType::Empty));
            arm.push(Ins::LocalGet(0));
            arm.push(Ins::LocalGet(1));
            arm.push(ctx.call(Rt::Len));
            arm.push(Ins::Return);
            arm.push(Ins::End);
        }
        // The write has to come first: after the trap there is no guest
        // instruction left to run, which is the same argument `alloc` makes
        // about recording heap exhaustion before failing. Key before code,
        // the order `record_thrown_string` uses.
        if ctx.string_member {
            arm.push(Ins::I32Const(FAULT_THROWN));
            arm.push(Ins::LocalGet(key));
            arm.push(Ins::I32Store(2, 0));
            store_fault(FAULT_MISSING_STRING_METHOD, &mut arm);
        } else {
            store_fault(FAULT_CAPABILITY, &mut arm);
        }
        arm.push(Ins::Unreachable);
        arm.push(Ins::End);
        f.body.extend(arm);
    }

    unbox_object(0, &mut f.body);
    f.body.push(Ins::LocalSet(o));

    let b = &mut f.body;
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_LEN));
    b.push(Ins::LocalGet(key));
    b.push(ctx.call(Rt::ObjFind));
    b.push(Ins::LocalSet(idx));

    b.push(Ins::LocalGet(idx));
    b.push(Ins::I32Const(-1));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    const_undefined(b);
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalSet(e));
    entry_at(b, e, idx);
    b.push(Ins::LocalSet(e));
    // The pair as it was stored, tag then payload -- not re-boxed, because
    // re-boxing would mean deciding a type the record already recorded.
    b.push(Ins::LocalGet(e));
    b.push(Ins::I32Load(ALIGN_WORD, ENTRY_TAG));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I64Load(ALIGN_WORD, ENTRY_PAYLOAD));
    f
}

/// `__obj_set(value, key, value)`: ECMA-262 10.1.9.2 OrdinarySet, over an
/// object with no prototype and no accessors.
///
/// A property that is already there is **overwritten where it is**: 10.1.9.2
/// changes the value of the existing descriptor and nothing else, which is
/// what keeps `{ a: 1, b: 2 }` with `o.a = 9` in the order `a`, `b`. A
/// property that is not there is **appended**, which is what makes property
/// order creation order (10.1.11.1 OrdinaryOwnPropertyKeys).
///
/// The receiver is tested for Object first, for the reason [`obj_get`] gives.
fn obj_set(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH + 1);
    let key = WIDTH;
    let tag = WIDTH + 1;
    let payload = WIDTH + 2;
    let o = f.local(ValType::I32);
    let idx = f.local(ValType::I32);
    let e = f.local(ValType::I32);
    let cap = f.local(ValType::I32);
    let src = f.local(ValType::I32);
    let dst = f.local(ValType::I32);
    let i = f.local(ValType::I32);

    unbox_object(0, &mut f.body);
    f.body.push(Ins::LocalSet(o));

    let b = &mut f.body;
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_LEN));
    b.push(Ins::LocalGet(key));
    b.push(ctx.call(Rt::ObjFind));
    b.push(Ins::LocalSet(idx));

    // Already there: overwrite in place, position untouched.
    b.push(Ins::LocalGet(idx));
    b.push(Ins::I32Const(-1));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalSet(e));
    entry_at(b, e, idx);
    b.push(Ins::LocalSet(e));
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(tag));
    b.push(Ins::I32Store(ALIGN_WORD, ENTRY_TAG));
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(payload));
    b.push(Ins::I64Store(ALIGN_WORD, ENTRY_PAYLOAD));
    b.push(Ins::Return);
    b.push(Ins::End);

    // Not there: append, growing the vector first if it is full.
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_LEN));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_CAP));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_CAP));
    b.push(Ins::I32Const(GROWTH));
    b.push(Ins::I32Mul);
    b.push(Ins::LocalSet(cap));
    b.push(Ins::LocalGet(cap));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(FIRST_CAP));
    b.push(Ins::LocalSet(cap));
    b.push(Ins::End);
    b.push(Ins::LocalGet(cap));
    b.push(Ins::I32Const(ENTRY_BYTES));
    b.push(Ins::I32Mul);
    b.push(ctx.call(Rt::Alloc));
    b.push(Ins::LocalSet(dst));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalSet(src));
    // Word by word, not byte by byte: an entry is four aligned words and the
    // old vector came from the same allocator, so there is no unaligned tail
    // to worry about. The old vector is left behind -- the heap has no free.
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(i));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_LEN));
    b.push(Ins::I32Const(ENTRY_BYTES));
    b.push(Ins::I32Mul);
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(src));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::I32Store(ALIGN_WORD, 0));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::LocalGet(o));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::I32Store(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalGet(o));
    b.push(Ins::LocalGet(cap));
    b.push(Ins::I32Store(ALIGN_WORD, OBJ_CAP));
    b.push(Ins::End);

    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalSet(e));
    // The append index is `len`. Read into `i` -- which the copy loop has
    // finished with -- because `entry_at` wants it in a local, and because the
    // count has to be read again to store `len + 1`.
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_LEN));
    b.push(Ins::LocalSet(i));
    entry_at(b, e, i);
    b.push(Ins::LocalSet(e));
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(key));
    b.push(Ins::I32Store(ALIGN_WORD, ENTRY_KEY));
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(tag));
    b.push(Ins::I32Store(ALIGN_WORD, ENTRY_TAG));
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(payload));
    b.push(Ins::I64Store(ALIGN_WORD, ENTRY_PAYLOAD));
    b.push(Ins::LocalGet(o));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::I32Store(ALIGN_WORD, OBJ_LEN));
    f
}

/// `__to_string(value) -> i32`: ECMA-262 7.1.17 ToString, over the five
/// primitive types, answering with a string record.
///
/// This is also 7.1.19 ToPropertyKey. Step 1 passes a Symbol through
/// unchanged and there are no Symbols here, so ToPropertyKey *reduces* to
/// ToString and the two are one function rather than two that agree. It used
/// to be called `__to_key`, because a computed member access was the only
/// caller; `+` is now the other one.
///
/// # Dispatch order
///
/// String **first**, which departs from `repr`'s documented Number-then-String
/// order, and the reason is the same shape as the reason for that order: this
/// site's dominant input is a String. Every computed access whose key is
/// already a string -- `o[k]` where `k` came out of another object, and the
/// literal `o["a"]` -- pays one test instead of two, and the Number path pays
/// a whole decimal conversion afterwards, so one extra test in front of it is
/// noise. Only this function and [`obj_get`] depart, and only here is it
/// written down twice.
///
/// # The Number arm is the whole algorithm now
///
/// It calls `__num_to_string`, [`super::convert`]'s shortest-round-tripping
/// 6.1.6.1.20. The integer-only `__num_to_str` this arm used to call is gone
/// with it: it existed because the general algorithm did not, and keeping a
/// second Number::toString that is exact on a subset would be two answers to
/// one question. `o[0.5]`, `o[NaN]` and `o[1/0]` therefore name the properties
/// `"0.5"`, `"NaN"` and `"Infinity"`, which is what the spec says and what
/// three tests used to record as a divergence.
fn to_string(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);

    is_string(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_string(0, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    is_number(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_number(0, &mut f.body);
    f.body.push(ctx.num_to_string());
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    let names = ctx.prim_names;
    is_bool(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_bool(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::I32Const(names.yes));
    f.body.push(Ins::Return);
    f.body.push(Ins::End);
    f.body.push(Ins::I32Const(names.no));
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    for (test, at) in [
        (is_null as fn(u32, &mut Vec<Ins>), names.null),
        (is_undefined, names.undefined),
    ] {
        test(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        f.body.push(Ins::I32Const(at));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);
    }

    // An Object or a Function would need 7.1.1 ToPrimitive, which needs the
    // `toString`/`valueOf` a prototype would carry, and there is no prototype.
    // The one conversion this milestone did *not* bring in, and the reason
    // `"" + {}` and `o[{}]` both still trap.
    f.body.push(Ins::Unreachable);
    f
}

// ---- functions -----------------------------------------------------------

/// A function record: `[element: i32]`, and nothing else yet.
///
/// One word, on the same bump heap every string and object lives on. It holds
/// the index of the module's table element whose adapter calls this function.
///
/// # Why there is a record at all
///
/// The payload used to *be* the element index, which made two evaluations of
/// one FunctionExpression one function value -- ECMA-262 15.2.5 says they are
/// two objects, and `mk() === mk()` answered `true`. One address per
/// evaluation is what makes them two, and the allocator already hands out one
/// address per allocation, so identity comes out of the existing `i64.eq` on
/// the payload with no arm added anywhere. See [`super::repr`]'s header.
///
/// # Why it is one word and not more
///
/// A spec function object also carries `length`, `name`, `prototype` and a
/// `[[Environment]]`. None of those is reachable here: there is no prototype,
/// so `f.length` and `f.name` are absent properties rather than wrong ones,
/// and there are no closures, so there is nothing to capture. Each is one more
/// word in this record when it lands, and the record is where it goes -- which
/// is the other thing an address buys that an index could not.
pub(crate) const FN_ELEMENT: u32 = 0;

/// Where a function record keeps its environment pointer, when the program
/// has closures at all.
///
/// Zero for a function that captures nothing, and the word is absent
/// altogether from a program in which nothing captures -- see `emit`'s
/// closure gate. This is the "one more word in this record when it lands"
/// [`FN_ELEMENT`]'s own doc predicted.
pub(crate) const FN_ENV: u32 = 4;

/// How many bytes one function record takes: the element, plus the
/// environment pointer once anything in the program captures.
pub(crate) const FN_BYTES: i32 = 4;
pub(crate) const FN_BYTES_WITH_ENV: i32 = 8;

/// `__fn_new(element) -> i32`: one fresh function record.
///
/// Called once per *evaluation* of a function expression and once per
/// instantiation of a function declaration, which is what 15.2.5 and 10.2.11
/// respectively ask for.
fn fn_new(ctx: &Ctx) -> FnBuild {
    // Two parameters once the program has closures -- element and environment
    // -- and one when it does not. The signature is built from the same flag
    // in `one`, so the two cannot disagree.
    let mut f = FnBuild::new(if ctx.captures { 2 } else { 1 });
    let p = f.local(ValType::I32);
    let b = &mut f.body;
    b.push(Ins::I32Const(if ctx.captures {
        FN_BYTES_WITH_ENV
    } else {
        FN_BYTES
    }));
    b.push(ctx.call(Rt::Alloc));
    b.push(Ins::LocalTee(p));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Store(ALIGN_WORD, FN_ELEMENT));
    if ctx.captures {
        b.push(Ins::LocalGet(p));
        b.push(Ins::LocalGet(1));
        b.push(Ins::I32Store(ALIGN_WORD, FN_ENV));
    }
    b.push(Ins::LocalGet(p));
    f
}

// ---- the string literal pool -------------------------------------------

/// Where the module's string literals go, and where the bump heap starts after
/// them. The record format is the same one `__alloc` hands out, so a literal
/// and a concatenation result are indistinguishable to everything above.
#[derive(Debug, Clone, Default)]
pub(crate) struct StringPool {
    at: Vec<(String, i32)>,
    bytes: Vec<u8>,
}

impl StringPool {
    /// Intern one literal and return its guest address. Equal literals share
    /// one record: the pool is the one place strings *are* interned, because
    /// here it is free -- the compiler already has both spellings in hand.
    /// `__str_concat` cannot do the same, which is why `__str_eq` compares
    /// bytes rather than pointers.
    pub(crate) fn intern(&mut self, text: &str) -> i32 {
        if let Some((_, at)) = self.at.iter().find(|(s, _)| s == text) {
            return *at;
        }
        let at = (DATA_ORIGIN + self.bytes.len() as u32) as i32;
        self.at.push((text.to_string(), at));
        self.bytes
            .extend_from_slice(&(text.len() as u32).to_le_bytes());
        self.bytes.extend_from_slice(text.as_bytes());
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        at
    }

    /// Place raw bytes and return their guest address, word-aligned.
    ///
    /// Not interned: a blob is placed by whoever needs it, once, and the
    /// caller keeps the address. Used for the lowercase table, which is data
    /// the guest binary-searches rather than a value the language can name.
    pub(crate) fn blob(&mut self, bytes: &[u8]) -> i32 {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        let at = (DATA_ORIGIN + self.bytes.len() as u32) as i32;
        self.bytes.extend_from_slice(bytes);
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        at
    }

    /// Whether the pool would emit no data segment.
    ///
    /// Asks the **bytes**, not the interned-string list: a blob leaves the
    /// list empty while filling the segment, and answering from the list would
    /// drop the segment and leave every address in it pointing at nothing.
    pub(crate) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The active data segment: its offset and its bytes.
    pub(crate) fn segment(&self) -> (u32, &[u8]) {
        (DATA_ORIGIN, &self.bytes)
    }

    /// The bump pointer's initial value -- the first free address after the
    /// literals.
    pub(crate) fn heap_start(&self) -> i32 {
        (DATA_ORIGIN + self.bytes.len() as u32) as i32
    }
}

// ---- function construction ----------------------------------------------

/// A function under construction: the parameters are fixed, the locals grow.
pub(crate) struct FnBuild {
    pub(crate) param_words: u32,
    pub(crate) extra: Vec<ValType>,
    pub(crate) body: Vec<Ins>,
}

impl FnBuild {
    pub(crate) fn new(param_words: u32) -> Self {
        Self {
            param_words,
            extra: Vec::new(),
            body: Vec::new(),
        }
    }

    pub(crate) fn local(&mut self, ty: ValType) -> u32 {
        let index = self.param_words + self.extra.len() as u32;
        self.extra.push(ty);
        index
    }

    /// Reserve one JS value's worth of locals and return its base index.
    pub(crate) fn value_local(&mut self) -> u32 {
        let base = self.param_words + self.extra.len() as u32;
        self.extra.extend_from_slice(&repr::SLOTS);
        base
    }

    /// Run-length groups, as the code section wants them.
    pub(crate) fn local_groups(&self) -> Vec<(u32, ValType)> {
        let mut groups: Vec<(u32, ValType)> = Vec::new();
        for ty in &self.extra {
            match groups.last_mut() {
                Some((n, prev)) if prev == ty => *n += 1,
                _ => groups.push((1, *ty)),
            }
        }
        groups
    }
}
