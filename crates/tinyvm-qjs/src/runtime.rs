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

use super::repr::{
    self, BlockType, Ins, ValType, WIDTH, box_bool, box_number, box_string, const_bool,
    const_string, is_bool, is_null, is_nullish, is_number, is_string, is_undefined, load_local,
    same_type, store_local, unbox_bool, unbox_number, unbox_string,
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
        }
    }
}

/// What the runtime needs to know about the module it is being spliced into.
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
}

impl Ctx {
    /// The call every lowering site emits.
    pub(crate) fn call(&self, rt: Rt) -> Ins {
        Ins::Call(self.func_base + rt.offset())
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
        (is_null, names.object),
    ] {
        test(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        const_string(at, &mut f.body);
        f.body.push(Ins::Return);
        f.body.push(Ins::End);
    }
    f.body.push(Ins::Unreachable);
    f
}

/// `+`: ECMA-262 13.15.3, ApplyStringOrNumericBinaryOperator.
///
/// No operand can be an Object yet, so `ToPrimitive` is the identity and the
/// spec reduces to: if either side is a String, concatenate the `ToString`s;
/// otherwise add the `ToNumber`s.
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

    is_string(0, &mut f.body);
    is_string(WIDTH, &mut f.body);
    f.body.push(Ins::I32Or);
    f.body.push(Ins::If(BlockType::Empty));
    is_string(0, &mut f.body);
    is_string(WIDTH, &mut f.body);
    f.body.push(Ins::I32And);
    f.body.push(Ins::If(BlockType::Empty));
    let mut inner = Vec::new();
    unbox_string(0, &mut inner);
    unbox_string(WIDTH, &mut inner);
    inner.push(ctx.call(Rt::StrConcat));
    box_string(&inner, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);
    // One side is a String and the other is not: ToString of a Number, a
    // Boolean, null or undefined is not implemented yet.
    f.body.push(Ins::Unreachable);
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

    is_string(0, &mut f.body);
    is_string(WIDTH, &mut f.body);
    f.body.push(Ins::I32Or);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::Unreachable);
    f.body.push(Ins::End);

    let mut inner = Vec::new();
    to_number_of(ctx, 0, &mut inner);
    to_number_of(ctx, WIDTH, &mut inner);
    inner.push(op);
    box_bool(&inner, &mut f.body);
    f
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

    is_string(0, &mut f.body);
    is_string(WIDTH, &mut f.body);
    f.body.push(Ins::I32Or);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::Unreachable);
    f.body.push(Ins::End);

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
fn to_number(_ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);

    is_number(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    unbox_number(0, &mut f.body);
    f.body.push(Ins::Return);
    f.body.push(Ins::End);

    // 7.1.4.1 StringToNumber: the StringNumericLiteral grammar is not
    // implemented, so a String reaching arithmetic is a hard stop.
    is_string(0, &mut f.body);
    f.body.push(Ins::If(BlockType::Empty));
    f.body.push(Ins::Unreachable);
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

    // Every tag is accounted for above, so reaching here means the pair was
    // not built by this engine.
    f.body.push(Ins::Unreachable);
    f
}

/// ECMA-262 7.1.2, ToBoolean. Complete over the five types.
///
/// `+0`, `-0` and `NaN` are the falsy Numbers, which is why the Number arm
/// needs the value three times and therefore a scratch `f64` local.
fn truthy(_ctx: &Ctx) -> FnBuild {
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

    f.body.push(Ins::Unreachable);
    f
}

/// `.length` of a String, as a Number. Traps on anything else, from
/// `unbox_string`: there is no property lookup yet, so the only way to reach
/// this is a lowering that already decided the receiver should be a String.
fn length(_ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let mut inner = Vec::new();
    unbox_string(0, &mut inner);
    inner.push(Ins::I32Load(2, 0));
    inner.push(Ins::F64ConvertI32S);
    box_number(&inner, &mut f.body);
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

    pub(crate) fn is_empty(&self) -> bool {
        self.at.is_empty()
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
