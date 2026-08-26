//! V1: the JavaScript value, as `(tag: i32, payload: i64)`.
//!
//! One JS value is two wasm values everywhere: two parameters per argument, two
//! results (wasm multi-value, which tinyvm supports, so nothing goes through
//! memory), two locals per variable, two operand slots on the stack.
//!
//! The payload is `i64` rather than `i32` because ECMA-262 6.1.6.1 says a
//! Number is an IEEE-754 double, and a double has to fit without a second
//! indirection. A wasm32 heap pointer is an `i32` zero-extended into the same
//! field.
//!
//! This layout is not a guess. It was chosen against NaN-boxing by a measured
//! experiment -- `plan/design-value-representation-experiment.md` in the
//! agenterm repo, results in `research/value-representation/RESULTS.md` -- and
//! the code below is lifted from that experiment's proven `src/repr_pair.rs`.
//! The deviations are listed in this crate's `tests/repr_v1.rs` header.
//!
//! # The tag domain
//!
//! One tag per ECMA-262 language type this engine has. The numbering is part of
//! the contract, not an implementation detail:
//!
//! | tag | type      | payload                                    |
//! |-----|-----------|--------------------------------------------|
//! | 0   | Undefined | always 0                                   |
//! | 1   | Number    | the double's bits, via `i64.reinterpret_f64`|
//! | 2   | Boolean   | 0 or 1                                     |
//! | 3   | String    | guest pointer to `[len: i32][utf8 bytes]`  |
//! | 4   | Null      | always 0                                   |
//! | 5   | Object    | guest pointer to an object record          |
//! | 6   | Function  | guest pointer to a function record         |
//! | 7   | Array     | guest pointer to a dense element vector    |
//!
//! `TAG_OBJECT` is 5 because it was the next free number when objects landed,
//! and appending is the whole point: see *Dispatch order* below. `TAG_FUNCTION`
//! is 6 for the same reason.
//!
//! A Function's payload **is** a heap pointer, like a String's and an
//! Object's, and it used to be the table index directly. That was the shorter
//! answer and it was wrong: see *The payload-0 invariant* below. The record it
//! points at holds the element index, because wasm MVP has no first-class
//! function reference an `i64` could hold -- the only thing a call can act on
//! is an index into the module's table, and `call_indirect` is the instruction
//! that acts on it. [`super::emit`] assigns the index when a function first
//! becomes a value, and element 0 is left null so that a zeroed word is never
//! a callable element.
//!
//! `TAG_UNDEFINED` is 0 so that a zero-initialised pair -- a fresh wasm local,
//! a zeroed word of linear memory -- reads as `undefined`, which is exactly
//! what an uninitialised binding means in JavaScript. Nothing may depend on
//! that accidentally: it is why the number is 0 and why it will stay 0.
//!
//! `TAG_NULL` is 4 rather than being slotted next to `TAG_UNDEFINED`, because
//! the four tags below it are the ones the experiment measured and renumbering
//! them would silently invalidate every number in `RESULTS.md`.
//!
//! # The payload-0 invariant
//!
//! Undefined and Null carry payload 0, always. That is what lets
//! [`super::runtime`]'s strict equality collapse "same tag, and neither is a
//! Number nor a String" into one `i64.eq` on the payloads: Booleans compare as
//! 0/1, and Undefined/Null compare as 0/0. Any future producer of those two
//! values must keep the payload at 0.
//!
//! Object arrived into that same arm and belongs there for a *different*
//! reason, which is worth stating so nobody "fixes" it: two Objects are
//! strictly equal exactly when they are the same object (ECMA-262 7.2.15 step
//! 4, SameValueNonNumber), the payload is the object's address, and a bump
//! allocator hands out one address per object. So the existing `i64.eq` is
//! already reference identity, and no arm was added for it.
//!
//! Function arrived into that same arm, and the argument for it was wrong the
//! first time. It read: "one function gets one element index however many
//! times it is read, and two function expressions get two, so the payload
//! comparison already is 7.2.15 step 4." Both halves are true and the sentence
//! is a proof only if those are the two cases. They are not. The third is one
//! function *expression evaluated twice* -- `function mk() { return function
//! () {}; } mk() === mk()` -- which ECMA-262 15.2.5 makes two objects and the
//! element index made one. The word "read" is what hid it, and
//! `tests/indirect_attack.rs` is what found it.
//!
//! So the payload is the address of a per-evaluation record, and the arm is
//! right for the reason the Object arm is right: a bump allocator hands out
//! one address per object, and the existing `i64.eq` is reference identity.
//! `===` still needed no new arm; what it needed was for the payload to be
//! the identity rather than the implementation.
//!
//! # Dispatch order
//!
//! Every operator that inspects its operands' types pays one type test per arm
//! it walks past. The experiment measured that cost directly (sensitivity
//! S-ADD in `RESULTS.md`): with `__add` testing String before Number, adding
//! Strings to the language cost 2 619 extra steps on a corpus that never used
//! one; with Number tested first, it cost zero. The verdict held either way, so
//! the ordering is free to be chosen on cost.
//!
//! **The order is Number, then String, then everything else, everywhere.** It
//! is stated once, here, so that a new type is added by appending an arm and
//! existing call sites keep paying what they paid. When a dispatch site departs
//! from this order it says why at the site.
//!
//! Object was added under that rule and paid what the rule promises: every arm
//! for it is **appended last**, after Boolean, Undefined and Null, so no
//! existing type executes one more test than it did before objects existed. An
//! Object itself pays the whole ladder at those sites -- and that is the right
//! side of the trade, because the sites are `__typeof`, `__truthy` and
//! `__to_number`, none of which an object-heavy script runs in a loop. The
//! sites objects *do* run hot -- property get and set -- are functions of
//! their own that test the receiver first, and say so there.
//!
//! Function was added under the same rule and cost the same: one arm appended
//! last in `__typeof`, `__truthy` and `__to_number`, and nothing anywhere else.
//! The site a function value *does* run hot is the call, and that site is not
//! a dispatch ladder at all -- it is one [`require_tag`] against
//! `TAG_FUNCTION`, which is one test whatever the tag domain grows to. That is
//! the measured growth law's best case, and it is worth saying why it is
//! reachable here and not at `__add`: a call has exactly one callable type to
//! accept, where an operator has a matrix of operand pairs to sort.

use tinyvm::Val;

// -------------------------------------------------------------------------
// The `super::ir` extension V1 requires.
//
// These types belong in `ir.rs`, next to the `Ins` they extend -- V1 needs
// `i64`, `f64`, structured control flow, a global and a linear memory, and
// `ir.rs` today has only the `i32` arithmetic M0 shipped. They live here
// because `ir.rs` is outside this lane's file domain. Moving them is one cut
// and paste plus one `use` line in each of `repr.rs` and `runtime.rs`; the
// variant names already follow `ir.rs`'s rule of being named after the wasm
// opcode rather than the JavaScript operator.
// -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValType {
    I32,
    I64,
    F64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockType {
    Empty,
}

/// One instruction. Named after the wasm opcode it becomes.
///
/// A superset of `ir.rs`'s `Ins`, including the four M0 variants V1 itself
/// never emits, so that folding this into `ir.rs` is a union and not a merge.
#[allow(dead_code)]
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
    /// `call_indirect(type, table)`: pop an element index off the stack, look
    /// the funcref up in the table, and call it only if its signature is
    /// *exactly* the named type (spec 4.4.8).
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
    // i64
    I64Eq,
    // f64
    F64Eq,
    F64Ne,
    F64Lt,
    F64Gt,
    F64Le,
    F64Ge,
    F64Abs,
    F64Neg,
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
    F64ReinterpretI64,
    I64ReinterpretF64,
}

// -------------------------------------------------------------------------
// The representation itself.
// -------------------------------------------------------------------------

pub(crate) const TAG_UNDEFINED: i32 = 0;
pub(crate) const TAG_NUMBER: i32 = 1;
pub(crate) const TAG_BOOL: i32 = 2;
pub(crate) const TAG_STRING: i32 = 3;
pub(crate) const TAG_NULL: i32 = 4;
/// An Object: the payload is a guest pointer to the record
/// [`super::runtime`] lays out. Appended rather than slotted in, for the
/// reason the module header's *Dispatch order* section gives.
pub(crate) const TAG_OBJECT: i32 = 5;
/// A Function: the payload is a guest pointer to the function record
/// [`super::runtime`] lays out, which holds the index of an element of the
/// module's funcref table. Calling it is a load and then `call_indirect`.
/// Appended rather than slotted in, for the reason the module header's
/// *Dispatch order* section gives.
pub(crate) const TAG_FUNCTION: i32 = 6;
/// An Array: the payload is a guest pointer to the dense element vector
/// [`super::array`] lays out at `ARR_HEADER`. Appended rather than slotted in,
/// for the reason the module header's *Dispatch order* section gives.
///
/// It is a tag of its own and not an Object with integer-named properties,
/// which is what ECMA-262 10.4.2 makes an Array and what the object record
/// could already have expressed. That record stores `[key][tag][payload]` and
/// finds a key by walking the entries with `__str_eq`, so `a[i]` under that
/// spelling would allocate a key string per access -- through Dragon4, since
/// the index is a Number -- and then scan. A dense vector reads element `i`
/// with one bounds test and one multiply-add. An array's whole reason to
/// exist is that the index *is* the address.
pub(crate) const TAG_ARRAY: i32 = 7;

/// The wasm value types one JS value occupies, in stack order.
pub(crate) const SLOTS: [ValType; 2] = [ValType::I32, ValType::I64];

/// How many wasm values one JS value is. Every `base` in this module is a
/// local index; the tag is at `base` and the payload at `base + 1`.
pub(crate) const WIDTH: u32 = SLOTS.len() as u32;

/// A JavaScript value as the host sees it, at the call boundary.
///
/// `String` is a guest pointer, not text: resolving it needs the instance's
/// memory, which is the caller's to hold, not this module's.
/// There is deliberately no `Object` variant and no `Function` one. The public `crate::Value` this
/// mirrors has none either, and adding one here without adding one there is a
/// half-open door: a host would receive a pointer it has no layout for and no
/// way to keep alive. [`host_decode`] therefore refuses the tag by name, which
/// is a sentence a host can act on, rather than by an "unknown tag" it cannot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HostVal {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    String(i32),
}

// ---- constructors -------------------------------------------------------
//
// A constructor takes its payload as an already-built instruction run rather
// than expecting it on the stack, because a two-word value is built bottom-up
// -- tag first, then payload -- and a stack-based constructor would force a
// scratch local for no reason other than the API shape.

/// `inner` leaves exactly one `f64` on the stack. Result: one JS Number.
pub(crate) fn box_number(inner: &[Ins], out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_NUMBER));
    out.extend_from_slice(inner);
    out.push(Ins::I64ReinterpretF64);
}

/// `inner` leaves exactly one `i32`, 0 or 1. Result: one JS Boolean.
pub(crate) fn box_bool(inner: &[Ins], out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_BOOL));
    out.extend_from_slice(inner);
    out.push(Ins::I64ExtendI32U);
}

/// `inner` leaves exactly one `i32` guest pointer. Result: one JS String.
pub(crate) fn box_string(inner: &[Ins], out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_STRING));
    out.extend_from_slice(inner);
    out.push(Ins::I64ExtendI32U);
}

/// `inner` leaves exactly one `i32` guest pointer to an object record.
/// Result: one JS Object.
///
/// Unused when this module is compiled without `emit`, which is what
/// `tests/repr_v1.rs` does -- the only producer of an Object is an object
/// literal, and that lives in the lowering.
#[allow(dead_code)]
pub(crate) fn box_object(inner: &[Ins], out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_OBJECT));
    out.extend_from_slice(inner);
    out.push(Ins::I64ExtendI32U);
}

/// `inner` leaves exactly one `i32` guest pointer to a function record.
/// Result: one JS Function.
///
/// Not a constant, which is the whole of the fix for the identity defect: a
/// function value has per-evaluation state after all, and that state is its
/// own address. ECMA-262 15.2.5 makes each evaluation of a FunctionExpression
/// a new object, and the only thing that can tell two of them apart here is
/// that the allocator handed out two addresses.
///
/// Unused when this module is compiled without `emit`, which is what
/// `tests/repr_v1.rs` does -- the only producer of a function value is the
/// lowering, because only the lowering knows which element a function took.
#[allow(dead_code)]
pub(crate) fn box_function(inner: &[Ins], out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_FUNCTION));
    out.extend_from_slice(inner);
    out.push(Ins::I64ExtendI32U);
}

/// `inner` leaves exactly one `i32` guest pointer to an array record.
/// Result: one JS Array.
///
/// Unused when this module is compiled without `emit`, for the reason
/// [`box_object`] gives.
#[allow(dead_code)]
pub(crate) fn box_array(inner: &[Ins], out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_ARRAY));
    out.extend_from_slice(inner);
    out.push(Ins::I64ExtendI32U);
}

/// Bit-exact: the payload is the double's bits, so nothing is lost -- not the
/// sign of a zero, not a NaN's payload.
pub(crate) fn const_number(value: f64, out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_NUMBER));
    out.push(Ins::I64Const(value.to_bits() as i64));
}

pub(crate) fn const_bool(value: bool, out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_BOOL));
    out.push(Ins::I64Const(i64::from(value)));
}

/// A string literal, which lives in the data segment and needs no allocation.
pub(crate) fn const_string(pointer: i32, out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_STRING));
    out.push(Ins::I64Const(i64::from(pointer)));
}

pub(crate) fn const_undefined(out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_UNDEFINED));
    out.push(Ins::I64Const(0));
}

pub(crate) fn const_null(out: &mut Vec<Ins>) {
    out.push(Ins::I32Const(TAG_NULL));
    out.push(Ins::I64Const(0));
}

// ---- accessors, reading the value held in the locals at `base` ----------
//
// Accessors read from *locals*, not from the stack. Type dispatch looks at a
// value more than once -- `is_string(a) && is_string(b)`, then
// `unbox_string(a)` -- and a stack-only accessor would force a spill. Every JS
// value that gets inspected is already a parameter or a local.

/// -> `f64`. Traps when the value is not a Number.
pub(crate) fn unbox_number(base: u32, out: &mut Vec<Ins>) {
    require_tag(base, TAG_NUMBER, out);
    out.push(Ins::LocalGet(base + 1));
    out.push(Ins::F64ReinterpretI64);
}

/// -> `i32`, 0 or 1. Traps when the value is not a Boolean.
pub(crate) fn unbox_bool(base: u32, out: &mut Vec<Ins>) {
    require_tag(base, TAG_BOOL, out);
    out.push(Ins::LocalGet(base + 1));
    out.push(Ins::I32WrapI64);
}

/// -> `i32` guest pointer. Traps when the value is not a String.
pub(crate) fn unbox_string(base: u32, out: &mut Vec<Ins>) {
    require_tag(base, TAG_STRING, out);
    out.push(Ins::LocalGet(base + 1));
    out.push(Ins::I32WrapI64);
}

/// -> `i32` guest pointer to an object record. Traps when the value is not an
/// Object -- which is what makes `undefined.a` a fault and not a fabricated
/// answer.
pub(crate) fn unbox_object(base: u32, out: &mut Vec<Ins>) {
    require_tag(base, TAG_OBJECT, out);
    out.push(Ins::LocalGet(base + 1));
    out.push(Ins::I32WrapI64);
}

/// -> `i32` guest pointer to an array record. Traps when the value is not an
/// Array, for the reason [`unbox_object`] gives about `undefined.a`.
///
/// Unused without `emit`, for the reason [`box_array`] gives.
#[allow(dead_code)]
pub(crate) fn unbox_array(base: u32, out: &mut Vec<Ins>) {
    require_tag(base, TAG_ARRAY, out);
    out.push(Ins::LocalGet(base + 1));
    out.push(Ins::I32WrapI64);
}

/// -> `i32` guest pointer to a function record. Traps when the value is not a
/// Function -- which is what makes `undefined()` a clean fault, raised before
/// any table is touched, rather than a `call_indirect` into whatever the
/// payload was.
///
/// It also traps when the payload does not *fit* in the `i32` the pointer is,
/// and that second check is the one worth explaining. Every other accessor
/// here narrows with a bare `i32.wrap_i64`, which silently discards the high
/// half. For a String or an Object the worst that buys is a wrong address,
/// which reads guest-owned bytes or traps on the bounds check. For a Function
/// it buys a *call*: `(TAG_FUNCTION, 2^32 + 1)` truncated to 1 lands wherever
/// element 1 points, quietly. `tests/indirect_attack.rs` verified that before
/// this check existed. No script can build such a pair -- the only producer is
/// the lowering -- and `Value` has no Function variant, so the only door left
/// is a hand-built `&[Val]` handed to `invoke_by_name`. The guard is here
/// anyway, because "no producer makes one" is a property of every producer
/// and this is a property of the consumer.
///
/// Written as extend-and-compare rather than a high-half test, because
/// [`Ins`] has `i64.eq` and no other 64-bit comparison.
///
/// Unused without `emit`, for the reason [`box_function`] gives.
#[allow(dead_code)]
pub(crate) fn unbox_function(base: u32, out: &mut Vec<Ins>) {
    require_tag(base, TAG_FUNCTION, out);
    out.push(Ins::LocalGet(base + 1));
    out.push(Ins::I32WrapI64);
    out.push(Ins::I64ExtendI32U);
    out.push(Ins::LocalGet(base + 1));
    out.push(Ins::I64Eq);
    out.push(Ins::I32Eqz);
    out.push(Ins::If(BlockType::Empty));
    out.push(Ins::Unreachable);
    out.push(Ins::End);
    out.push(Ins::LocalGet(base + 1));
    out.push(Ins::I32WrapI64);
}

pub(crate) fn is_number(base: u32, out: &mut Vec<Ins>) {
    tag_is(base, TAG_NUMBER, out);
}

pub(crate) fn is_string(base: u32, out: &mut Vec<Ins>) {
    tag_is(base, TAG_STRING, out);
}

pub(crate) fn is_bool(base: u32, out: &mut Vec<Ins>) {
    tag_is(base, TAG_BOOL, out);
}

pub(crate) fn is_undefined(base: u32, out: &mut Vec<Ins>) {
    tag_is(base, TAG_UNDEFINED, out);
}

pub(crate) fn is_null(base: u32, out: &mut Vec<Ins>) {
    tag_is(base, TAG_NULL, out);
}

pub(crate) fn is_object(base: u32, out: &mut Vec<Ins>) {
    tag_is(base, TAG_OBJECT, out);
}

pub(crate) fn is_function(base: u32, out: &mut Vec<Ins>) {
    tag_is(base, TAG_FUNCTION, out);
}

pub(crate) fn is_array(base: u32, out: &mut Vec<Ins>) {
    tag_is(base, TAG_ARRAY, out);
}

/// `null` or `undefined` -- the one pair ECMA-262 7.2.14 lets `==` bridge.
///
/// Two tag tests and an `or`, not the one-instruction `tag & ~TAG_NULL == 0`
/// that the chosen numbering happens to permit. That trick reads as arithmetic
/// and means nothing; it would also break silently the first time a tag is
/// added that happens to satisfy it.
pub(crate) fn is_nullish(base: u32, out: &mut Vec<Ins>) {
    is_undefined(base, out);
    is_null(base, out);
    out.push(Ins::I32Or);
}

/// The two tags are equal exactly when the two values have the same ECMA-262
/// language type, because there is one tag per type.
pub(crate) fn same_type(left: u32, right: u32, out: &mut Vec<Ins>) {
    out.push(Ins::LocalGet(left));
    out.push(Ins::LocalGet(right));
    out.push(Ins::I32Eq);
}

// ---- moving whole values ------------------------------------------------

/// Push every word of the JS value held at local `base`.
pub(crate) fn load_local(base: u32, out: &mut Vec<Ins>) {
    for k in 0..WIDTH {
        out.push(Ins::LocalGet(base + k));
    }
}

/// Pop one JS value off the stack into the locals starting at `base`.
pub(crate) fn store_local(base: u32, out: &mut Vec<Ins>) {
    for k in (0..WIDTH).rev() {
        out.push(Ins::LocalSet(base + k));
    }
}

/// Drop one JS value from the stack -- what an expression statement does with
/// the value it computed. Nothing calls it until the lowering grows statements.
#[allow(dead_code)]
pub(crate) fn drop_value(out: &mut Vec<Ins>) {
    for _ in 0..WIDTH {
        out.push(Ins::Drop);
    }
}

// ---- the host door ------------------------------------------------------

/// Encode a host-supplied JS value into the call ABI.
pub(crate) fn host_encode(value: HostVal) -> [Val; 2] {
    let (tag, payload) = match value {
        HostVal::Undefined => (TAG_UNDEFINED, 0),
        HostVal::Null => (TAG_NULL, 0),
        HostVal::Number(x) => (TAG_NUMBER, x.to_bits() as i64),
        HostVal::Bool(b) => (TAG_BOOL, i64::from(b)),
        HostVal::String(p) => (TAG_STRING, i64::from(p)),
    };
    [Val::I32(tag), Val::I64(payload)]
}

/// Decode what a compiled function returned. The error is a message rather
/// than a [`super::diag::CompileError`]: nothing here is a property of the
/// user's source, it is a broken guest or a host that called the wrong export.
pub(crate) fn host_decode(vals: &[Val]) -> Result<HostVal, String> {
    let [Val::I32(tag), Val::I64(payload)] = vals else {
        return Err(format!(
            "V1 expects a (i32, i64) pair back, got {} value(s)",
            vals.len()
        ));
    };
    let bits = *payload as u64;
    Ok(match *tag {
        TAG_UNDEFINED => HostVal::Undefined,
        TAG_NULL => HostVal::Null,
        TAG_NUMBER => HostVal::Number(f64::from_bits(bits)),
        TAG_BOOL => HostVal::Bool(bits != 0),
        TAG_STRING => HostVal::String(bits as u32 as i32),
        // Named rather than lumped into "unknown": the guest is fine and the
        // value is real, but an Object is a reference into a heap the host has
        // no layout for and no way to keep alive. A host that sees this
        // sentence knows to ask the script for a property instead.
        TAG_OBJECT => {
            return Err(
                "V1: an Object is a guest heap reference; `Value` has no variant for one yet"
                    .to_string(),
            );
        }
        // Named rather than lumped into "unknown", for the reason the Object
        // arm gives, and for one more: a function value points at a record
        // holding an index into *this module's* table, so it has no meaning at
        // all on the other side of the boundary -- not even one a host could
        // learn.
        TAG_FUNCTION => {
            return Err(
                "V1: a function is a guest reference to an index into this module's own table; `Value` has no variant for one"
                    .to_string(),
            );
        }
        other => return Err(format!("V1: unknown tag {other}")),
    })
}

// ---- internals ----------------------------------------------------------

fn tag_is(base: u32, tag: i32, out: &mut Vec<Ins>) {
    out.push(Ins::LocalGet(base));
    out.push(Ins::I32Const(tag));
    out.push(Ins::I32Eq);
}

/// `if (tag != want) unreachable` -- the trap an accessor raises on a type it
/// was not handed. Unreachable rather than a fabricated value: a wrong number
/// that flows on is indistinguishable from a real one, which is the silent
/// corruption this stack refuses (see the `emit` module on division by zero).
fn require_tag(base: u32, tag: i32, out: &mut Vec<Ins>) {
    tag_is(base, tag, out);
    out.push(Ins::I32Eqz);
    out.push(Ins::If(BlockType::Empty));
    out.push(Ins::Unreachable);
    out.push(Ins::End);
}
