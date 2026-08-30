//! Arrays: the eighth tag, its dense element vector, and the property
//! dispatch that reaches it.
//!
//! Designed in `plan/design-array-milestone.md`, which records why each of the
//! four decisions below is the one taken and what the two rejected answers
//! were. The short version, because a reader here should not have to leave:
//!
//! 1. **An eighth tag, not an Object with integer keys.** The object record
//!    could already express `a[0]` -- keys are Strings and ECMA-262 7.1.19
//!    makes `o[1]` and `o["1"]` one slot -- but finding one walks the entries
//!    with `__str_eq` after `__num_to_string` has run Dragon4 to build the key.
//!    A dense vector reads element `i` with one bounds test and one
//!    multiply-add. An array's reason to exist is that the index *is* the
//!    address.
//! 2. **This set is gated**, like [`super::convert`]'s JSON set and unlike
//!    [`super::runtime`]'s, whose `SET` is unconditional. A program with no
//!    array literal and no `JSON` emits none of these functions -- see
//!    [`SET`]'s own comment for why that predicate is exact.
//! 3. **The index fast path is reached only from a Computed key.** A Static
//!    key is an IdentifierName and never a canonical numeric string, so
//!    `o.a` keeps calling `__obj_get` directly and pays nothing for arrays
//!    existing. That distinction is a property of the grammar, which is the
//!    form `emit`'s `key()` blesses; recognising an *array receiver* at a call
//!    site is the per-site exemption it forbids.
//! 4. **There are no holes.** See [`arr_set`].

use super::repr::{
    self, BlockType, Ins, TAG_ARRAY, TAG_NUMBER, TAG_OBJECT, ValType, WIDTH, const_undefined,
};
use super::runtime::{
    ALIGN_WORD, FAULT_INVALID_WRITE, FnBuild, RefusalNames, Rt, RtFunc, StringPool,
    record_named_fault,
};

// -------------------------------------------------------------------------
// The record.
// -------------------------------------------------------------------------

/// `[len: i32][cap: i32][elems: i32]`, the same three-word head the object
/// record has at `OBJ_HEADER`, so the growth code below is the same shape as
/// `obj_set`'s and can be read against it.
pub(crate) const ARR_HEADER: i32 = 12;
pub(crate) const ARR_LEN: u32 = 0;
pub(crate) const ARR_CAP: u32 = 4;
pub(crate) const ARR_ELEMS: u32 = 8;

/// One element: `[tag: i32][payload: i64]`.
///
/// The V1 pair stored whole, tag beside payload, for the reason `ENTRY_TAG`
/// gives about a property: a read is two loads and not a re-boxing, which
/// would mean deciding a type the record already recorded.
///
/// Twelve bytes, so the payload of every odd element sits at an address that
/// is 4 mod 8. `ALIGN_WORD` is 2 and the object record already declares
/// below-natural alignment at `ENTRY_PAYLOAD`; it is legal wasm and a hint
/// only.
pub(crate) const ELEM_BYTES: i32 = 12;
pub(crate) const ELEM_TAG: u32 = 0;
pub(crate) const ELEM_PAYLOAD: u32 = 4;

/// The vector an array allocates the first time it has to grow, and the factor
/// afterwards. A literal is built at its exact size (`__arr_new` takes the
/// count), so these are only reached by an array filled with `a[i] = v`.
const FIRST_CAP: i32 = 4;
const GROWTH: i32 = 2;

/// The largest index this engine will address, and the reason it is a limit
/// rather than `i32::MAX`.
///
/// `a[i] = v` past the end fills the gap ([`arr_set`]), so an index is also an
/// allocation of `index * ELEM_BYTES` bytes. Multiplying an unchecked `i32` by
/// 12 overflows into a *small* positive number, which would turn a huge index
/// into a write near the start of the heap -- silent corruption, and the exact
/// shape this stack refuses. Anything at or above this is refused before the
/// multiply; a real script never reaches it, and one that does gets a fault
/// rather than a wrong answer.
const MAX_INDEX: i32 = 1 << 24;

// -------------------------------------------------------------------------
// The set.
// -------------------------------------------------------------------------

/// The emitted array functions, in index order. Position in [`SET`] is the
/// offset from [`Ctx::func_base`], exactly as `Rt` is for `runtime::SET`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ar {
    New,
    Grow,
    Push,
    Get,
    Set,
    /// The index a key denotes, or `-1` for a key that is not one.
    Index,
    PropGet,
    PropSet,
}

/// Every array function, in module order.
///
/// **Emitted only when the program can produce an array**, which is exactly:
/// it contains an ArrayLiteral node, or it names `JSON`. `JSON` is in the
/// predicate because `JSON.parse` builds an array out of text the compiler
/// never sees. Nothing else can bring one into existence -- a computed access
/// in a program with neither can never find an array to index -- so the
/// predicate is exact, which is the property `convert`'s JSON gate is chosen
/// for and the property a gate has to have to be worth having.
pub(crate) const SET: &[Ar] = &[
    Ar::New,
    Ar::Grow,
    Ar::Push,
    Ar::Get,
    Ar::Set,
    Ar::Index,
    Ar::PropGet,
    Ar::PropSet,
];

impl Ar {
    /// The name the function is given in the module. Not exported, for the
    /// reason `Rt::symbol` gives.
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Ar::New => "__arr_new",
            Ar::Grow => "__arr_grow",
            Ar::Push => "__arr_push",
            Ar::Get => "__arr_get",
            Ar::Set => "__arr_set",
            Ar::Index => "__arr_index",
            Ar::PropGet => "__prop_get",
            Ar::PropSet => "__prop_set",
        }
    }

    /// Offset of this function from [`Ctx::func_base`].
    pub(crate) fn offset(self) -> u32 {
        SET.iter()
            .position(|a| *a == self)
            .expect("SET lists every Ar") as u32
    }
}

/// The one fixed String this set names.
///
/// Interned here rather than in the lowering because this set is what reads
/// it. A second interning of the same text elsewhere would be the same record
/// -- `StringPool::intern` shares -- but a second place to keep right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Names {
    pub(crate) length: i32,
}

impl Names {
    pub(crate) fn intern(pool: &mut StringPool) -> Self {
        Self {
            length: pool.intern("length"),
        }
    }
}

/// What this set needs to name the functions it calls.
#[derive(Debug, Clone)]
pub(crate) struct Ctx {
    /// Index of `__arr_new`.
    pub(crate) func_base: u32,
    /// Index of `__add` -- the base `runtime::SET` is laid out from.
    pub(crate) runtime_base: u32,
    pub(crate) names: Names,
    /// The reasons a refused write names; `None` in a program that cannot
    /// reach one (see `emit`).
    pub(crate) refusal_names: Option<RefusalNames>,
}

impl Ctx {
    fn me(&self, ar: Ar) -> Ins {
        Ins::Call(self.func_base + ar.offset())
    }

    fn rt(&self, rt: Rt) -> Ins {
        Ins::Call(self.runtime_base + rt.offset())
    }
}

/// Build every array function, in [`SET`] order. Called only for a program the
/// gate admits.
pub(crate) fn build(ctx: &Ctx) -> Vec<RtFunc> {
    SET.iter().map(|ar| one(ctx, *ar)).collect()
}

fn values(n: usize) -> Vec<ValType> {
    (0..n).flat_map(|_| repr::SLOTS).collect()
}

fn one(ctx: &Ctx, ar: Ar) -> RtFunc {
    let i32_ = ValType::I32;
    let i64_ = ValType::I64;
    let (params, results, f) = match ar {
        Ar::New => (vec![i32_], vec![i32_], arr_new(ctx)),
        Ar::Grow => (vec![i32_], vec![], arr_grow(ctx)),
        Ar::Push => (vec![i32_, i32_, i64_], vec![], arr_push(ctx)),
        Ar::Get => (vec![i32_, i32_], values(1), arr_get()),
        Ar::Set => (vec![i32_, i32_, i32_, i64_], vec![], arr_set(ctx)),
        Ar::Index => (values(1), vec![i32_], arr_index()),
        Ar::PropGet => (values(2), values(1), prop_get(ctx)),
        Ar::PropSet => (values(3), vec![], prop_set(ctx)),
    };
    RtFunc {
        name: ar.symbol(),
        params,
        results,
        locals: f.local_groups(),
        body: f.body,
    }
}

// -------------------------------------------------------------------------
// The record's own operations.
// -------------------------------------------------------------------------

/// `__arr_new(cap) -> i32`: an empty array with room for `cap` elements.
///
/// The capacity is the caller's, for the reason `obj_new` gives: a literal has
/// its elements counted at compile time and never reallocates. `__arr_new(0)`
/// allocates no vector at all, which is what `[]` wants.
fn arr_new(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let p = f.local(ValType::I32);
    let b = &mut f.body;
    b.push(Ins::I32Const(ARR_HEADER));
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(p));
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32Const(0));
    b.push(Ins::I32Store(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Store(ALIGN_WORD, ARR_CAP));
    // Written before the test rather than in an `else`, for the reason
    // `obj_new` gives: there is no `else` in this instruction set and a zero
    // pointer is the honest value for "no vector".
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32Const(0));
    b.push(Ins::I32Store(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalGet(0));
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::I32Store(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::End);
    b.push(Ins::LocalGet(p));
    f
}

/// `__arr_grow(a)`: make room for at least one more element.
///
/// Its own function rather than inline in both callers, which is the one place
/// this file departs from `obj_set`'s shape -- that one grows inline because
/// it has exactly one append site. There are two here (`__arr_push` and
/// `__arr_set` past the end), and duplicating a copy loop is how the two
/// quietly stop agreeing.
///
/// The old vector is left behind: the heap has no free.
fn arr_grow(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let cap = f.local(ValType::I32);
    let dst = f.local(ValType::I32);
    let src = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_CAP));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_CAP));
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
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(dst));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalSet(src));

    // Word by word: an element is three aligned words and the old vector came
    // from the same allocator, so there is no unaligned tail.
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(i));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::I32Const(ELEM_BYTES));
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

    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::I32Store(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(cap));
    b.push(Ins::I32Store(ALIGN_WORD, ARR_CAP));
    f
}

/// `__arr_push(a, tag, payload)`: append one element, growing first if full.
fn arr_push(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(3);
    let e = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(0));
    b.push(ctx.me(Ar::Grow));

    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalSet(n));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(e));

    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32Store(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(2));
    b.push(Ins::I64Store(ALIGN_WORD, ELEM_PAYLOAD));

    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::I32Store(ALIGN_WORD, ARR_LEN));
    f
}

/// `__arr_get(a, index) -> value`: the element, or `undefined` past the end.
///
/// `undefined` and not a fault, for the reason `obj_get` gives about an absent
/// property: 10.1.8.1 with a null prototype answers `undefined`, and `a[i]`
/// inside a bounds test the script wrote itself is the single most ordinary
/// thing a script does with an array.
///
/// The bounds test is unsigned, so a negative index is caught by the same
/// comparison rather than by a second one.
fn arr_get() -> FnBuild {
    let mut f = FnBuild::new(2);
    let e = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(1));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::I32GeU);
    b.push(Ins::If(BlockType::Empty));
    const_undefined(b);
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(e));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
    f
}

/// `__arr_set(a, index, tag, payload)`: write one element, extending the array
/// to reach it.
///
/// # There are no holes, and that is a claim about what is observable
///
/// ECMA-262 makes `a[5] = 1` on a length-2 array produce length 6 with four
/// *holes*, and a hole differs from `undefined` only through `in`,
/// `hasOwnProperty`, `Object.keys` and the iteration methods that skip it.
/// This engine has none of them, so filling the gap with `undefined` is
/// indistinguishable from the specified behaviour for every script it can run.
///
/// It stops being indistinguishable the day one of those arrives. That is why
/// this paragraph exists rather than a shorter comment: whoever adds `in` or
/// `forEach` needs to find it.
///
/// An index at or above [`MAX_INDEX`] is refused before the multiply that
/// would overflow -- see that constant.
fn arr_set(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(4);
    let e = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(1));
    b.push(Ins::I32Const(MAX_INDEX));
    b.push(Ins::I32GeU);
    b.push(Ins::If(BlockType::Empty));
    // Nameless on purpose: every script path reaches this through
    // `__arr_index`, which refused the index first. Only a caller inside the
    // engine could arrive here, so this is a defect and not the script's.
    b.push(Ins::Unreachable);
    b.push(Ins::End);

    // Extend one element at a time rather than in one allocation: `__arr_push`
    // already owns growth, and reaching for the allocator here would be a
    // second place that has to agree with it about capacity.
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(0));
    const_undefined(b);
    b.push(ctx.me(Ar::Push));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    // At the end exactly: append, which is also how `length` reaches
    // `index + 1`.
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(2));
    b.push(Ins::LocalGet(3));
    b.push(ctx.me(Ar::Push));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(e));
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(2));
    b.push(Ins::I32Store(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(3));
    b.push(Ins::I64Store(ALIGN_WORD, ELEM_PAYLOAD));
    f
}

/// `__arr_index(key) -> i32`: the array index this key denotes, or `-1`.
///
/// Only a Number is one here. ECMA-262 10.4.2.1 works on the *String* form and
/// accepts `a["0"]` as index 0, which this does not -- a string key falls
/// through to the `length`-or-`undefined` path in [`prop_get`], so
/// `a["0"]` reads `undefined` where the spec reads element 0.
///
/// That is a recorded divergence and not an oversight. Closing it means
/// running the canonical-numeric-string test (7.1.21 CanonicalNumericIndexString)
/// on every string key of every array access, and the population this milestone
/// exists for -- `JSON.parse` of a broker answer, then `a[i]` in a `for` loop --
/// never writes one. `a[0]` with a Number is the whole of what is needed and is
/// what this fast-paths.
///
/// The guards are in this order because each makes the next safe:
/// `I32TruncF64S` traps on NaN and on anything outside `i32`, so the range test
/// comes before it, and the integrality test compares the truncation against
/// the original rather than testing the fraction.
fn arr_index() -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let d = f.local(ValType::F64);
    let i = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Const(TAG_NUMBER));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(-1));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(1));
    b.push(Ins::F64ReinterpretI64);
    b.push(Ins::LocalSet(d));

    // `!(d >= 0)` rather than `d < 0`, so NaN -- which answers false to both --
    // is refused here and never reaches the truncation.
    b.push(Ins::LocalGet(d));
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Ge);
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(-1));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(d));
    b.push(Ins::F64Const(MAX_INDEX as f64));
    b.push(Ins::F64Ge);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(-1));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(d));
    b.push(Ins::I32TruncF64S);
    b.push(Ins::LocalSet(i));
    b.push(Ins::LocalGet(i));
    b.push(Ins::F64ConvertI32S);
    b.push(Ins::LocalGet(d));
    b.push(Ins::F64Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(-1));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(i));
    f
}

// -------------------------------------------------------------------------
// Property dispatch.
// -------------------------------------------------------------------------

/// `__prop_get(receiver, key) -> value`: a computed member access, over an
/// Object or an Array.
///
/// # Dispatch order
///
/// Object **first**, which departs from `repr`'s documented
/// Number-then-String order for the reason `obj_get` already records at its
/// own receiver test: in every non-erroneous program the receiver of a
/// property access is an Object, so testing anything before it puts a test in
/// front of the only path that ever succeeds. An array receiver pays that one
/// extra test, which is the right side of the trade -- the alternative charges
/// every object access instead.
///
/// The Object arm is byte-for-byte what a Computed access did before arrays
/// existed: `__to_string` then `__obj_get`. Nothing about an object's cost
/// changed; the conversion simply moved from the call site to here.
fn prop_get(ctx: &Ctx) -> FnBuild {
    let recv = 0;
    let key = WIDTH;
    let mut f = FnBuild::new(2 * WIDTH);
    let a = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let k = f.local(ValType::I32);
    let b = &mut f.body;

    // -- Everything that is not an Array is `obj_get`'s question, and one
    // test asks it. That includes the Object receiver this used to name
    // separately, the String receiver whose `.length` `obj_get` answers, and
    // `undefined[k]` / `null[k]`, which are the TypeErrors `obj_get`'s own
    // receiver test raises and stay one trap.
    b.push(Ins::LocalGet(recv));
    b.push(Ins::I32Const(TAG_ARRAY));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(recv));
    b.push(Ins::LocalGet(recv + 1));
    b.push(Ins::LocalGet(key));
    b.push(Ins::LocalGet(key + 1));
    b.push(ctx.rt(Rt::ToStr));
    b.push(ctx.rt(Rt::ObjGet));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(recv + 1));
    b.push(Ins::I32WrapI64);
    b.push(Ins::LocalSet(a));

    // -- The index fast path: no string is built and none is compared.
    b.push(Ins::LocalGet(key));
    b.push(Ins::LocalGet(key + 1));
    b.push(ctx.me(Ar::Index));
    b.push(Ins::LocalSet(i));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(-1));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(a));
    b.push(Ins::LocalGet(i));
    b.push(ctx.me(Ar::Get));
    b.push(Ins::Return);
    b.push(Ins::End);

    // -- `a.length`, and then every other name.
    b.push(Ins::LocalGet(key));
    b.push(Ins::LocalGet(key + 1));
    b.push(ctx.rt(Rt::ToStr));
    b.push(Ins::LocalSet(k));
    b.push(Ins::LocalGet(k));
    b.push(Ins::I32Const(ctx.names.length));
    b.push(ctx.rt(Rt::StrEq));
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(TAG_NUMBER));
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::F64ConvertI32S);
    b.push(Ins::I64ReinterpretF64);
    b.push(Ins::Return);
    b.push(Ins::End);

    // Absent, not a fault -- for the reason `obj_get` gives.
    const_undefined(b);
    f
}

/// `__prop_set(receiver, key, value)`: a computed member assignment.
///
/// Object first, for the reason [`prop_get`] gives.
///
/// # A non-index property on an array is refused, not faked
///
/// `a.foo = 1` has nowhere to go: the record is a dense vector with no key
/// space. It traps rather than being dropped, because a dropped write is a
/// value the script believes it stored and will read back as `undefined`
/// later, somewhere else, with nothing pointing at the assignment that lost
/// it.
///
/// Giving the array record a second, general property store to make it work is
/// the temptation `plan/design-array-milestone.md` names as the disease this
/// milestone must **detect** rather than satisfy. It is recorded here as a
/// finding: the day a real script needs `a.foo`, that is the decision to
/// reopen, deliberately.
///
/// `a.length = 0` traps under the same arm. Truncation is a real thing scripts
/// do and it is worth its own answer eventually; it does not get a fabricated
/// one now.
fn prop_set(ctx: &Ctx) -> FnBuild {
    let recv = 0;
    let key = WIDTH;
    let val = 2 * WIDTH;
    let mut f = FnBuild::new(3 * WIDTH);
    let i = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(recv));
    b.push(Ins::I32Const(TAG_OBJECT));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(recv));
    b.push(Ins::LocalGet(recv + 1));
    b.push(Ins::LocalGet(key));
    b.push(Ins::LocalGet(key + 1));
    b.push(ctx.rt(Rt::ToStr));
    b.push(Ins::LocalGet(val));
    b.push(Ins::LocalGet(val + 1));
    b.push(ctx.rt(Rt::ObjSet));
    b.push(Ins::Return);
    b.push(Ins::End);

    // Not an Object and not an Array: a String, a Number, a Boolean, `null`,
    // `undefined` or a function. Nothing here has a property to write.
    b.push(Ins::LocalGet(recv));
    b.push(Ins::I32Const(TAG_ARRAY));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    if let Some(names) = ctx.refusal_names {
        record_named_fault(names.write_on_primitive, FAULT_INVALID_WRITE, b);
    }
    b.push(Ins::Unreachable);
    b.push(Ins::End);

    // An Array, and a key `__arr_index` could not read as an integer index
    // (a String, a fraction, a negative): ECMA-262 would hang a named
    // property on the array; this engine refuses and says so.
    b.push(Ins::LocalGet(key));
    b.push(Ins::LocalGet(key + 1));
    b.push(ctx.me(Ar::Index));
    b.push(Ins::LocalSet(i));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(-1));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    if let Some(names) = ctx.refusal_names {
        record_named_fault(names.non_index_key, FAULT_INVALID_WRITE, b);
    }
    b.push(Ins::Unreachable);
    b.push(Ins::End);

    b.push(Ins::LocalGet(recv + 1));
    b.push(Ins::I32WrapI64);
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(val));
    b.push(Ins::LocalGet(val + 1));
    b.push(ctx.me(Ar::Set));
    f
}
