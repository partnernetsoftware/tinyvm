//! The three ECMA-262 conversions the runtime is missing, as emitted wasm.
//!
//! [`super::runtime`] carries an `unreachable` at each of three places, and
//! its header names them: `Number::toString` (6.1.6.1.20), `StringToNumber`
//! (7.1.4.1) and String relational comparison (7.2.13). This module is those
//! three algorithms. Nothing here is a host import: a script converts a number
//! to a string without a host watching, because the conversion is part of the
//! language and not part of the door.
//!
//! # Number::toString is the hard one
//!
//! 6.1.6.1.20 step 5 asks for the *shortest* decimal that reads back as the
//! same binary64, then the closest such decimal, then the even one. That is
//! three conditions, and each of them is load-bearing:
//!
//! - shortest is why `0.1` prints `0.1` and not `0.1000000000000000055511…`;
//! - closest is why `0.1 + 0.2` prints `0.30000000000000004`;
//! - even is why `785068460487425.25` prints `785068460487425.2` and not
//!   `…5.3` — the two are exactly equidistant, and the spec picks the even
//!   `s`. Rust's own shortest-round-trip formatter picks `…5.3` there, so the
//!   Rust oracle in `tests/conversions.rs` is checked *against the spec* at
//!   that point rather than believed.
//!
//! The algorithm is Steele & White's free-format Dragon4 in Burger & Dybvig's
//! formulation: hold the value as an exact rational `r/s` together with the
//! half-gaps `m+`/`m-` to its two neighbours, scale until the interval sits in
//! `(1/10, 1]`, then emit digits until the remaining interval is small enough
//! that one digit distinguishes the value from both neighbours. Every step is
//! exact integer arithmetic, so there is no rounding to be wrong about. The
//! `low`/`high` boundary tests are inclusive exactly when the significand is
//! even, which is the reader's round-half-to-even seen from this side.
//!
//! Two polarity traps live in that scaling loop and both are written at the
//! site: the `too_big` and `too_small` tests must be *mirror* strictnesses, or
//! they overlap at `(r + m+) * 10 == s` and the loop oscillates forever. `1e23`
//! is a value that reaches that point.
//!
//! # Why there is a bignum here
//!
//! `r`, `s`, `m+` and `m-` reach about 1085 bits for a subnormal, which is
//! four times what wasm's widest integer holds. So this module carries a small
//! multiple-precision integer: 16-bit limbs, one per `i32` word.
//!
//! 16 bits and not 32 because of the instruction set. Carry extraction is
//! `t >> 16`, and wasm's `i32.shr_u`/`i32.div_u` are not in this compiler's IR
//! yet ([`super::repr`]'s `Ins`, which is another lane's file). With 16-bit
//! limbs every intermediate — `limb * 10000 + carry` at worst — stays below
//! `2^31`, where `i32.div_s` *is* unsigned division and `i32.lt_s` *is*
//! unsigned comparison. The representation is chosen by what the available
//! opcodes make exact, which is the same reason `__rem` in the runtime is a
//! doubling loop rather than a division.
//!
//! Division shows up in only two shapes and neither needs a general one. In
//! Dragon4 the quotient is a single decimal digit, so it is at most nine
//! subtractions. In `StringToNumber` the quotient is 56 bits, so it is 56
//! shift-and-subtract steps against a divisor walking down. A schoolbook
//! multi-limb division is not needed for either.
//!
//! # The other two
//!
//! `StringToNumber` (7.1.4.1) is the whole `StrNumericLiteral` grammar:
//! whitespace, sign, `Infinity`, hex/octal/binary, and the empty or
//! whitespace-only string being `+0`. Its numeric core is exact for the same
//! reason: the decimal is held as an exact ratio of big integers and divided
//! once, so the result is the correctly rounded double and not an accumulated
//! product of roundings. There is one fast path and it is a theorem, not a
//! guess — Clinger 1990: when the significand is under `2^53` and the decimal
//! exponent is within `±22`, both operands of the single multiply or divide
//! are exact, so one IEEE operation is one rounding and therefore correctly
//! rounded. Fifteen digits and `|e| <= 22` is inside that.
//!
//! String relational comparison (7.2.13) is by **UTF-16 code unit**, which is
//! neither byte order nor code point order: a supplementary character is two
//! surrogates, and `U+D800` sorts below `U+E000`. UTF-8 byte order agrees with
//! code *point* order, so a byte compare answers `"\u{10000}" > ""` and
//! the spec answers the other way. So the comparison decodes.

use super::repr::{
    self, BlockType, Ins, TAG_FUNCTION, TAG_UNDEFINED, ValType, box_bool, box_function, box_number,
    box_object, box_string, const_null, const_undefined, is_bool, is_function, is_null, is_nullish,
    is_number, is_object, is_string, is_undefined, load_local, store_local, unbox_bool,
    unbox_number, unbox_object, unbox_string,
};
use super::runtime::{
    Ctx as RtCtx, ENTRY_BYTES, ENTRY_KEY, ENTRY_PAYLOAD, ENTRY_TAG, FnBuild, OBJ_ENTRIES, OBJ_LEN,
    Rt, RtFunc, STRING_HEADER, StringPool, record_uncaught_throw,
};

/// `i32.load` / `i32.store` alignment, as the exponent the memarg wants.
const ALIGN_WORD: u32 = 2;

/// A bignum record: `[cap: i32][len: i32][limb: i32 * cap]`.
///
/// `cap` and `len` are counted in limbs; a limb holds 16 bits in the low half
/// of an `i32` word. `len` is the number of significant limbs, so zero is
/// `len == 0` and the top limb is never zero.
const BN_CAP: u32 = 0;
const BN_LEN: u32 = 4;
const BN_LIMBS: u32 = 8;

/// Limbs reserved for one Dragon4 working value.
///
/// The largest thing the loop holds is `(r + m+) * 10` for a subnormal, where
/// `s` is `2^1075`; that is about 1085 bits. 80 limbs is 1280.
const D4_LIMBS: i32 = 80;

/// Limbs reserved for one `StringToNumber` working value.
///
/// The dividend after its pre-shift, or the divisor after the extra 55 bits
/// the division loop needs, reaches about 3680 bits — 768 kept digits is 2551
/// bits, times `10^1091` for the smallest exponent the range check lets
/// through. 260 limbs is 4160.
const S2N_LIMBS: i32 = 260;

/// Significant decimal digits kept from the source string.
///
/// 767 is the most a rounding boundary can have: a midpoint is
/// `(2m+1) * 2^-k` with `m < 2^53` and `k <= 1074`, whose decimal expansion is
/// `(2m+1) * 5^k / 10^k`, and `log10((2^54) * 5^1074)` is under 767. So past
/// that a truncated prefix and the true value are never on opposite sides of a
/// boundary, and one sticky bit carries everything the dropped tail can say.
const MAX_DIGITS: i32 = 768;

/// The emitted conversion functions, in index order. Position in [`SET`] is
/// the offset from [`Ctx::func_base`], exactly as [`Rt`] is for the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cv {
    // multiple precision
    BnNew,
    BnNorm,
    BnSeti,
    BnSet4,
    BnCopy,
    BnBits,
    BnMulSmall,
    BnAddSmall,
    BnMulPow10,
    BnShl,
    BnShr1,
    BnCmp,
    BnAdd,
    BnSub,
    // Number::toString
    Dragon4,
    NumToString,
    // StringToNumber
    WsLen,
    SkipWs,
    RatioToF64,
    DigitsToF64,
    StrToNum,
    // relational comparison
    U16Next,
    StrCmp,
}

/// Every conversion function, in the order they are defined in the module.
pub(crate) const SET: &[Cv] = &[
    Cv::BnNew,
    Cv::BnNorm,
    Cv::BnSeti,
    Cv::BnSet4,
    Cv::BnCopy,
    Cv::BnBits,
    Cv::BnMulSmall,
    Cv::BnAddSmall,
    Cv::BnMulPow10,
    Cv::BnShl,
    Cv::BnShr1,
    Cv::BnCmp,
    Cv::BnAdd,
    Cv::BnSub,
    Cv::Dragon4,
    Cv::NumToString,
    Cv::WsLen,
    Cv::SkipWs,
    Cv::RatioToF64,
    Cv::DigitsToF64,
    Cv::StrToNum,
    Cv::U16Next,
    Cv::StrCmp,
];

impl Cv {
    /// The name the function is given in the module. Not exported, for the
    /// reason [`Rt::symbol`] gives.
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Cv::BnNew => "__bn_new",
            Cv::BnNorm => "__bn_norm",
            Cv::BnSeti => "__bn_seti",
            Cv::BnSet4 => "__bn_set4",
            Cv::BnCopy => "__bn_copy",
            Cv::BnBits => "__bn_bits",
            Cv::BnMulSmall => "__bn_mul_small",
            Cv::BnAddSmall => "__bn_add_small",
            Cv::BnMulPow10 => "__bn_mul_pow10",
            Cv::BnShl => "__bn_shl",
            Cv::BnShr1 => "__bn_shr1",
            Cv::BnCmp => "__bn_cmp",
            Cv::BnAdd => "__bn_add",
            Cv::BnSub => "__bn_sub",
            Cv::Dragon4 => "__dragon4",
            Cv::NumToString => "__num_to_string",
            Cv::WsLen => "__ws_len",
            Cv::SkipWs => "__skip_ws",
            Cv::RatioToF64 => "__ratio_to_f64",
            Cv::DigitsToF64 => "__digits_to_f64",
            Cv::StrToNum => "__str_to_num",
            Cv::U16Next => "__u16_next",
            Cv::StrCmp => "__str_cmp",
        }
    }

    /// Offset of this function from [`Ctx::func_base`].
    pub(crate) fn offset(self) -> u32 {
        SET.iter()
            .position(|c| *c == self)
            .expect("SET lists every Cv") as u32
    }
}

/// The four fixed Strings 6.1.6.1.20 answers with before it reaches step 5, as
/// guest addresses in the module's string pool.
///
/// Pool records like any other literal, so a script's `"NaN"` and this one's
/// are the same record — [`StringPool::intern`] shares equal literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Names {
    /// Step 1.
    pub(crate) nan: i32,
    /// Step 4.
    pub(crate) infinity: i32,
    /// Step 3 over step 4. Interned whole rather than concatenated, because a
    /// concatenation would allocate to produce a constant.
    pub(crate) neg_infinity: i32,
    /// Step 2 — and it is `"0"` for `-0` as well as `+0`.
    pub(crate) zero: i32,
}

impl Names {
    pub(crate) fn intern(pool: &mut StringPool) -> Self {
        Self {
            nan: pool.intern("NaN"),
            infinity: pool.intern("Infinity"),
            neg_infinity: pool.intern("-Infinity"),
            zero: pool.intern("0"),
        }
    }
}

/// What this module needs to know about the module it is spliced into.
pub(crate) struct Ctx {
    /// Function index of `__bn_new` — the first of [`SET`].
    pub(crate) func_base: u32,
    /// Function index of `__add` — the first of [`super::runtime::SET`]. Only
    /// [`Rt::Alloc`] is reached through it; every allocation here is the same
    /// bump heap every other string lives on.
    pub(crate) runtime_base: u32,
    pub(crate) names: Names,
}

impl Ctx {
    /// The call every lowering site emits.
    pub(crate) fn call(&self, cv: Cv) -> Ins {
        Ins::Call(self.func_base + cv.offset())
    }

    fn alloc(&self) -> Ins {
        Ins::Call(self.runtime_base + Rt::Alloc.offset())
    }

    /// Build this module's `Ctx` from the runtime's, given where the two sets
    /// were placed. Kept so an integrator wiring `emit.rs` states the two
    /// bases once.
    #[allow(dead_code)]
    pub(crate) fn beside(rt: &RtCtx, func_base: u32, names: Names) -> Self {
        Self {
            func_base,
            runtime_base: rt.func_base,
            names,
        }
    }
}

/// Build every conversion function, in [`SET`] order.
pub(crate) fn build(ctx: &Ctx) -> Vec<RtFunc> {
    SET.iter().map(|cv| one(ctx, *cv)).collect()
}

fn one(ctx: &Ctx, cv: Cv) -> RtFunc {
    use ValType::{F64, I32};
    let (params, results, f) = match cv {
        Cv::BnNew => (vec![I32], vec![I32], bn_new(ctx)),
        Cv::BnNorm => (vec![I32], vec![], bn_norm()),
        Cv::BnSeti => (vec![I32, I32], vec![], bn_seti()),
        Cv::BnSet4 => (vec![I32; 5], vec![], bn_set4(ctx)),
        Cv::BnCopy => (vec![I32, I32], vec![], bn_copy()),
        Cv::BnBits => (vec![I32], vec![I32], bn_bits()),
        Cv::BnMulSmall => (vec![I32, I32], vec![], bn_mul_small()),
        Cv::BnAddSmall => (vec![I32, I32], vec![], bn_add_small()),
        Cv::BnMulPow10 => (vec![I32, I32], vec![], bn_mul_pow10(ctx)),
        Cv::BnShl => (vec![I32, I32], vec![], bn_shl()),
        Cv::BnShr1 => (vec![I32], vec![], bn_shr1(ctx)),
        Cv::BnCmp => (vec![I32, I32], vec![I32], bn_cmp()),
        Cv::BnAdd => (vec![I32, I32], vec![], bn_add(ctx)),
        Cv::BnSub => (vec![I32, I32], vec![], bn_sub(ctx)),
        Cv::Dragon4 => (vec![F64, I32], vec![I32, I32], dragon4(ctx)),
        Cv::NumToString => (vec![F64], vec![I32], num_to_string(ctx)),
        Cv::WsLen => (vec![I32, I32, I32], vec![I32], ws_len()),
        Cv::SkipWs => (vec![I32, I32, I32], vec![I32], skip_ws(ctx)),
        Cv::RatioToF64 => (vec![I32, I32, I32, I32], vec![F64], ratio_to_f64(ctx)),
        Cv::DigitsToF64 => (vec![I32, I32, I32, I32], vec![F64], digits_to_f64(ctx)),
        Cv::StrToNum => (vec![I32], vec![F64], str_to_num(ctx)),
        Cv::U16Next => (vec![I32, I32, I32], vec![I32; 3], u16_next()),
        Cv::StrCmp => (vec![I32, I32], vec![I32], str_cmp(ctx)),
    };
    RtFunc {
        name: cv.symbol(),
        params,
        results,
        locals: f.local_groups(),
        body: f.body,
    }
}

// -------------------------------------------------------------------------
// Emission helpers. Nothing here is an abstraction over wasm; they are
// abbreviations, so that an algorithm reads as the algorithm.
// -------------------------------------------------------------------------

fn ld(i: u32) -> Ins {
    Ins::LocalGet(i)
}
fn st(i: u32) -> Ins {
    Ins::LocalSet(i)
}
fn ic(v: i32) -> Ins {
    Ins::I32Const(v)
}

/// `block { loop { !cond -> br 1 ; body ; br 0 } }`.
///
/// `body` may contain balanced blocks of its own; it may not branch out of
/// this one, because the depths would be wrong.
fn while_loop(
    b: &mut Vec<Ins>,
    cond: impl FnOnce(&mut Vec<Ins>),
    body: impl FnOnce(&mut Vec<Ins>),
) {
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    cond(b);
    b.push(Ins::I32Eqz);
    b.push(Ins::BrIf(1));
    body(b);
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
}

fn if_then(b: &mut Vec<Ins>, cond: impl FnOnce(&mut Vec<Ins>), body: impl FnOnce(&mut Vec<Ins>)) {
    cond(b);
    b.push(Ins::If(BlockType::Empty));
    body(b);
    b.push(Ins::End);
}

/// Push the address of limb `i` of the bignum at local `p`.
fn limb_at(b: &mut Vec<Ins>, p: u32, i: u32) {
    b.extend_from_slice(&[ld(p), ld(i), ic(4), Ins::I32Mul, Ins::I32Add]);
}

/// Push limb `i` of `p`.
fn limb_get(b: &mut Vec<Ins>, p: u32, i: u32) {
    limb_at(b, p, i);
    b.push(Ins::I32Load(ALIGN_WORD, BN_LIMBS));
}

/// `p.limbs[i] = <value>`, where `value` leaves one `i32`.
fn limb_set(b: &mut Vec<Ins>, p: u32, i: u32, value: impl FnOnce(&mut Vec<Ins>)) {
    limb_at(b, p, i);
    value(b);
    b.push(Ins::I32Store(ALIGN_WORD, BN_LIMBS));
}

fn field_get(b: &mut Vec<Ins>, p: u32, off: u32) {
    b.push(ld(p));
    b.push(Ins::I32Load(ALIGN_WORD, off));
}

fn field_set(b: &mut Vec<Ins>, p: u32, off: u32, value: impl FnOnce(&mut Vec<Ins>)) {
    b.push(ld(p));
    value(b);
    b.push(Ins::I32Store(ALIGN_WORD, off));
}

/// `x < y`, signed. Every quantity compared in this module is non-negative and
/// under `2^31`, which is what makes the signed forms unsigned ones.
fn lt(b: &mut Vec<Ins>, x: u32, y: u32) {
    b.extend_from_slice(&[ld(x), ld(y), Ins::I32LtS]);
}

/// `local = local + delta`.
fn bump(b: &mut Vec<Ins>, local: u32, delta: i32) {
    b.extend_from_slice(&[ld(local), ic(delta), Ins::I32Add, st(local)]);
}

/// `if (!ok) unreachable` — a capacity claim checked rather than commented.
/// Reaching one is a defect in this module, not in the script, which is why it
/// is a trap and not a fabricated answer.
fn require(b: &mut Vec<Ins>, ok: impl FnOnce(&mut Vec<Ins>)) {
    ok(b);
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::Unreachable);
    b.push(Ins::End);
}

// -------------------------------------------------------------------------
// Multiple precision
// -------------------------------------------------------------------------

/// `__bn_new(cap) -> p`. `cap` limbs of 16 bits each, value zero.
///
/// The limb words are not cleared: `len` is zero, and every operation that
/// grows `len` writes the limbs it grows over. That invariant is what makes
/// this two stores instead of a loop over `cap`.
fn bn_new(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let p = f.local(ValType::I32);
    let b = &mut f.body;
    b.extend_from_slice(&[
        ld(0),
        ic(4),
        Ins::I32Mul,
        ic(BN_LIMBS as i32),
        Ins::I32Add,
        ctx.alloc(),
        st(p),
    ]);
    field_set(b, p, BN_CAP, |b| b.push(ld(0)));
    field_set(b, p, BN_LEN, |b| b.push(ic(0)));
    b.push(ld(p));
    f
}

/// `__bn_norm(p)`: drop top limbs that are zero, so that `len == 0` is the
/// only spelling of zero and `cmp` can compare lengths first.
fn bn_norm() -> FnBuild {
    let mut f = FnBuild::new(1);
    let n = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(n));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.extend_from_slice(&[ld(n), Ins::I32Eqz, Ins::BrIf(1)]);
    bump(b, n, -1);
    limb_get(b, 0, n);
    b.push(Ins::If(BlockType::Empty));
    // Non-zero: `n` is the index of the top limb, so the length is one more.
    bump(b, n, 1);
    b.push(Ins::Br(2));
    b.push(Ins::End);
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    field_set(b, 0, BN_LEN, |b| b.push(ld(n)));
    f
}

/// `__bn_seti(p, v)`: `p = v`, for `v` in `0 ..= 65535`.
fn bn_seti() -> FnBuild {
    let mut f = FnBuild::new(2);
    let b = &mut f.body;
    require(b, |b| {
        field_get(b, 0, BN_CAP);
        b.extend_from_slice(&[ic(0), Ins::I32LtS, Ins::I32Eqz]);
    });
    b.push(ld(0));
    b.push(ld(1));
    b.push(Ins::I32Store(ALIGN_WORD, BN_LIMBS));
    field_set(b, 0, BN_LEN, |b| {
        b.extend_from_slice(&[ld(1), ic(0), Ins::I32Ne]);
    });
    f
}

/// `__bn_set4(p, l0, l1, l2, l3)`: `p` = the four 16-bit limbs, little end
/// first. The only producer is a binary64 significand, which is 53 bits.
fn bn_set4(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(5);
    let b = &mut f.body;
    require(b, |b| {
        field_get(b, 0, BN_CAP);
        b.extend_from_slice(&[ic(3), Ins::I32LtS, Ins::I32Eqz]);
    });
    for k in 0..4u32 {
        b.extend_from_slice(&[ld(0), ic(4 * k as i32), Ins::I32Add, ld(1 + k)]);
        b.push(Ins::I32Store(ALIGN_WORD, BN_LIMBS));
    }
    field_set(b, 0, BN_LEN, |b| b.push(ic(4)));
    b.push(ld(0));
    b.push(ctx.call(Cv::BnNorm));
    f
}

/// `__bn_copy(dst, src)`.
fn bn_copy() -> FnBuild {
    let mut f = FnBuild::new(2);
    let n = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 1, BN_LEN);
    b.push(st(n));
    require(b, |b| {
        field_get(b, 0, BN_CAP);
        b.push(ld(n));
        b.push(Ins::I32LtS);
        b.push(Ins::I32Eqz);
    });
    b.extend_from_slice(&[ic(0), st(i)]);
    while_loop(
        b,
        |b| lt(b, i, n),
        |b| {
            limb_set(b, 0, i, |b| limb_get(b, 1, i));
            bump(b, i, 1);
        },
    );
    field_set(b, 0, BN_LEN, |b| b.push(ld(n)));
    f
}

/// `__bn_bits(p) -> the position of the highest set bit`, zero for zero.
fn bn_bits() -> FnBuild {
    let mut f = FnBuild::new(1);
    let n = f.local(ValType::I32);
    let t = f.local(ValType::I32);
    let c = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(n));
    b.push(ld(n));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(ic(0));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.extend_from_slice(&[ld(n), ic(1), Ins::I32Sub, st(n)]);
    limb_get(b, 0, n);
    b.push(st(t));
    b.extend_from_slice(&[ic(0), st(c)]);
    while_loop(
        b,
        |b| b.push(ld(t)),
        |b| {
            bump(b, c, 1);
            b.extend_from_slice(&[ld(t), ic(2), Ins::I32DivS, st(t)]);
        },
    );
    b.extend_from_slice(&[ld(n), ic(16), Ins::I32Mul, ld(c), Ins::I32Add]);
    f
}

/// `__bn_mul_small(p, m)` for `m <= 10000`.
///
/// `limb * m + carry` is under `65535 * 10000 + 10000`, which is under `2^31`,
/// which is why `i32.div_s` by 65536 is the carry and needs no unsigned shift.
fn bn_mul_small() -> FnBuild {
    let mut f = FnBuild::new(2);
    let n = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let carry = f.local(ValType::I32);
    let t = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(n));
    b.extend_from_slice(&[ic(0), st(i), ic(0), st(carry)]);
    while_loop(
        b,
        |b| lt(b, i, n),
        |b| {
            limb_get(b, 0, i);
            b.extend_from_slice(&[ld(1), Ins::I32Mul, ld(carry), Ins::I32Add, st(t)]);
            limb_set(b, 0, i, |b| {
                b.extend_from_slice(&[ld(t), ic(0xFFFF), Ins::I32And]);
            });
            b.extend_from_slice(&[ld(t), ic(65536), Ins::I32DivS, st(carry)]);
            bump(b, i, 1);
        },
    );
    while_loop(
        b,
        |b| b.push(ld(carry)),
        |b| {
            require(b, |b| {
                field_get(b, 0, BN_CAP);
                b.push(ld(n));
                b.push(Ins::I32LtS);
                b.push(Ins::I32Eqz);
            });
            limb_set(b, 0, n, |b| {
                b.extend_from_slice(&[ld(carry), ic(0xFFFF), Ins::I32And]);
            });
            b.extend_from_slice(&[ld(carry), ic(65536), Ins::I32DivS, st(carry)]);
            bump(b, n, 1);
        },
    );
    field_set(b, 0, BN_LEN, |b| b.push(ld(n)));
    f
}

/// `__bn_add_small(p, v)` for `v <= 65535`.
fn bn_add_small() -> FnBuild {
    let mut f = FnBuild::new(2);
    let n = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let carry = f.local(ValType::I32);
    let t = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(n));
    b.extend_from_slice(&[ic(0), st(i), ld(1), st(carry)]);
    while_loop(
        b,
        |b| b.push(ld(carry)),
        |b| {
            require(b, |b| {
                field_get(b, 0, BN_CAP);
                b.push(ld(i));
                b.push(Ins::I32LtS);
                b.push(Ins::I32Eqz);
            });
            // The running sum lives in a local, not on the stack: a block may
            // not reach below the height it was entered at, so "push, then add
            // to it inside an `if`" is not wasm even though it balances.
            b.extend_from_slice(&[ld(carry), st(t)]);
            if_then(
                b,
                |b| lt(b, i, n),
                |b| {
                    b.push(ld(t));
                    limb_get(b, 0, i);
                    b.extend_from_slice(&[Ins::I32Add, st(t)]);
                },
            );
            limb_set(b, 0, i, |b| {
                b.extend_from_slice(&[ld(t), ic(0xFFFF), Ins::I32And]);
            });
            b.extend_from_slice(&[ld(t), ic(65536), Ins::I32DivS, st(carry)]);
            bump(b, i, 1);
            if_then(
                b,
                |b| {
                    lt(b, n, i);
                },
                |b| b.extend_from_slice(&[ld(i), st(n)]),
            );
        },
    );
    field_set(b, 0, BN_LEN, |b| b.push(ld(n)));
    f
}

/// `__bn_mul_pow10(p, k)`: four decimal digits at a time, because `10^4` is
/// the largest power of ten a limb multiply keeps exact.
fn bn_mul_pow10(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2);
    let k = f.local(ValType::I32);
    let b = &mut f.body;
    b.extend_from_slice(&[ld(1), st(k)]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ic(3), ld(k), Ins::I32LtS]),
        |b| {
            b.extend_from_slice(&[ld(0), ic(10000), ctx.call(Cv::BnMulSmall)]);
            bump(b, k, -4);
        },
    );
    while_loop(
        b,
        |b| b.extend_from_slice(&[ic(0), ld(k), Ins::I32LtS]),
        |b| {
            b.extend_from_slice(&[ld(0), ic(10), ctx.call(Cv::BnMulSmall)]);
            bump(b, k, -1);
        },
    );
    f
}

/// `__bn_shl(p, bits)`.
fn bn_shl() -> FnBuild {
    let mut f = FnBuild::new(2);
    let n = f.local(ValType::I32);
    let lim = f.local(ValType::I32);
    let sh = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let carry = f.local(ValType::I32);
    let t = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(n));
    b.push(ld(n));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.extend_from_slice(&[ld(1), ic(16), Ins::I32DivS, st(lim)]);
    b.extend_from_slice(&[ld(1), ic(15), Ins::I32And, st(sh)]);
    if_then(
        b,
        |b| b.push(ld(sh)),
        |b| {
            b.extend_from_slice(&[ic(0), st(i), ic(0), st(carry)]);
            while_loop(
                b,
                |b| lt(b, i, n),
                |b| {
                    limb_get(b, 0, i);
                    b.extend_from_slice(&[ld(sh), Ins::I32Shl, ld(carry), Ins::I32Or, st(t)]);
                    limb_set(b, 0, i, |b| {
                        b.extend_from_slice(&[ld(t), ic(0xFFFF), Ins::I32And]);
                    });
                    b.extend_from_slice(&[ld(t), ic(65536), Ins::I32DivS, st(carry)]);
                    bump(b, i, 1);
                },
            );
            if_then(
                b,
                |b| b.push(ld(carry)),
                |b| {
                    require(b, |b| {
                        field_get(b, 0, BN_CAP);
                        b.push(ld(n));
                        b.push(Ins::I32LtS);
                        b.push(Ins::I32Eqz);
                    });
                    limb_set(b, 0, n, |b| b.push(ld(carry)));
                    bump(b, n, 1);
                },
            );
        },
    );
    if_then(
        b,
        |b| b.push(ld(lim)),
        |b| {
            require(b, |b| {
                field_get(b, 0, BN_CAP);
                b.extend_from_slice(&[ld(n), ld(lim), Ins::I32Add, Ins::I32LtS, Ins::I32Eqz]);
            });
            b.extend_from_slice(&[ld(n), st(i)]);
            while_loop(
                b,
                |b| b.push(ld(i)),
                |b| {
                    bump(b, i, -1);
                    // limbs[i + lim] = limbs[i]
                    b.extend_from_slice(&[
                        ld(0),
                        ld(i),
                        ld(lim),
                        Ins::I32Add,
                        ic(4),
                        Ins::I32Mul,
                        Ins::I32Add,
                    ]);
                    limb_get(b, 0, i);
                    b.push(Ins::I32Store(ALIGN_WORD, BN_LIMBS));
                },
            );
            b.extend_from_slice(&[ic(0), st(i)]);
            while_loop(
                b,
                |b| lt(b, i, lim),
                |b| {
                    limb_set(b, 0, i, |b| b.push(ic(0)));
                    bump(b, i, 1);
                },
            );
            b.extend_from_slice(&[ld(n), ld(lim), Ins::I32Add, st(n)]);
        },
    );
    field_set(b, 0, BN_LEN, |b| b.push(ld(n)));
    f
}

/// `__bn_shr1(p)`: one bit right. Only the division loop needs it, and only by
/// one, which is why there is no general right shift.
fn bn_shr1(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let n = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(n));
    b.push(ld(n));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.extend_from_slice(&[ic(0), st(i)]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ld(i), ic(1), Ins::I32Add, ld(n), Ins::I32LtS]),
        |b| {
            limb_set(b, 0, i, |b| {
                limb_get(b, 0, i);
                b.extend_from_slice(&[ic(2), Ins::I32DivS]);
                b.extend_from_slice(&[ld(0), ld(i), ic(1), Ins::I32Add, ic(4), Ins::I32Mul]);
                b.push(Ins::I32Add);
                b.push(Ins::I32Load(ALIGN_WORD, BN_LIMBS));
                b.extend_from_slice(&[ic(1), Ins::I32And, ic(15), Ins::I32Shl, Ins::I32Or]);
            });
            bump(b, i, 1);
        },
    );
    limb_set(b, 0, i, |b| {
        limb_get(b, 0, i);
        b.extend_from_slice(&[ic(2), Ins::I32DivS]);
    });
    b.push(ld(0));
    b.push(ctx.call(Cv::BnNorm));
    f
}

/// `__bn_cmp(a, b) -> -1 | 0 | 1`.
fn bn_cmp() -> FnBuild {
    let mut f = FnBuild::new(2);
    let na = f.local(ValType::I32);
    let nb = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let x = f.local(ValType::I32);
    let y = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(na));
    field_get(b, 1, BN_LEN);
    b.push(st(nb));
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(na), ld(nb), Ins::I32Ne]),
        |b| {
            lt(b, na, nb);
            b.push(Ins::If(BlockType::Empty));
            b.push(ic(-1));
            b.push(Ins::Return);
            b.push(Ins::End);
            b.push(ic(1));
            b.push(Ins::Return);
        },
    );
    b.extend_from_slice(&[ld(na), st(i)]);
    while_loop(
        b,
        |b| b.push(ld(i)),
        |b| {
            bump(b, i, -1);
            limb_get(b, 0, i);
            b.push(st(x));
            limb_get(b, 1, i);
            b.push(st(y));
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(x), ld(y), Ins::I32Ne]),
                |b| {
                    lt(b, x, y);
                    b.push(Ins::If(BlockType::Empty));
                    b.push(ic(-1));
                    b.push(Ins::Return);
                    b.push(Ins::End);
                    b.push(ic(1));
                    b.push(Ins::Return);
                },
            );
        },
    );
    b.push(ic(0));
    f
}

/// `__bn_add(a, b)`: `a += b`.
fn bn_add(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2);
    let na = f.local(ValType::I32);
    let nb = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let carry = f.local(ValType::I32);
    let t = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(na));
    field_get(b, 1, BN_LEN);
    b.push(st(nb));
    b.extend_from_slice(&[ic(0), st(i), ic(0), st(carry)]);
    while_loop(
        b,
        |b| {
            // i < nb || carry != 0 || i < na
            b.extend_from_slice(&[ld(i), ld(nb), Ins::I32LtS]);
            b.extend_from_slice(&[ld(i), ld(na), Ins::I32LtS, Ins::I32Or]);
            b.extend_from_slice(&[ld(carry), Ins::I32Or]);
        },
        |b| {
            require(b, |b| {
                field_get(b, 0, BN_CAP);
                b.push(ld(i));
                b.push(Ins::I32LtS);
                b.push(Ins::I32Eqz);
            });
            b.extend_from_slice(&[ld(carry), st(t)]);
            if_then(
                b,
                |b| lt(b, i, na),
                |b| {
                    b.push(ld(t));
                    limb_get(b, 0, i);
                    b.extend_from_slice(&[Ins::I32Add, st(t)]);
                },
            );
            if_then(
                b,
                |b| lt(b, i, nb),
                |b| {
                    b.push(ld(t));
                    limb_get(b, 1, i);
                    b.extend_from_slice(&[Ins::I32Add, st(t)]);
                },
            );
            limb_set(b, 0, i, |b| {
                b.extend_from_slice(&[ld(t), ic(0xFFFF), Ins::I32And]);
            });
            b.extend_from_slice(&[ld(t), ic(65536), Ins::I32DivS, st(carry)]);
            bump(b, i, 1);
        },
    );
    field_set(b, 0, BN_LEN, |b| b.push(ld(i)));
    b.push(ld(0));
    b.push(ctx.call(Cv::BnNorm));
    f
}

/// `__bn_sub(a, b)`: `a -= b`, for `a >= b`. A borrow out of the top is a
/// defect here, so it traps rather than wrapping.
fn bn_sub(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2);
    let na = f.local(ValType::I32);
    let nb = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let borrow = f.local(ValType::I32);
    let t = f.local(ValType::I32);
    let b = &mut f.body;
    field_get(b, 0, BN_LEN);
    b.push(st(na));
    field_get(b, 1, BN_LEN);
    b.push(st(nb));
    b.extend_from_slice(&[ic(0), st(i), ic(0), st(borrow)]);
    while_loop(
        b,
        |b| lt(b, i, na),
        |b| {
            limb_get(b, 0, i);
            b.extend_from_slice(&[ld(borrow), Ins::I32Sub, st(t)]);
            if_then(
                b,
                |b| lt(b, i, nb),
                |b| {
                    b.push(ld(t));
                    limb_get(b, 1, i);
                    b.extend_from_slice(&[Ins::I32Sub, st(t)]);
                },
            );
            b.extend_from_slice(&[ic(0), st(borrow)]);
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(t), ic(0), Ins::I32LtS]),
                |b| {
                    b.extend_from_slice(&[ld(t), ic(65536), Ins::I32Add, st(t)]);
                    b.extend_from_slice(&[ic(1), st(borrow)]);
                },
            );
            limb_set(b, 0, i, |b| b.push(ld(t)));
            bump(b, i, 1);
        },
    );
    require(b, |b| {
        b.extend_from_slice(&[ld(borrow), Ins::I32Eqz]);
    });
    b.push(ld(0));
    b.push(ctx.call(Cv::BnNorm));
    f
}

// -------------------------------------------------------------------------
// The binary64 field layout, read through memory
// -------------------------------------------------------------------------

/// Store the `f64` in local `x` at scratch `sc` and split it into
/// `(m0, m1, m2, m3)` -- the significand as four 16-bit limbs, implicit bit
/// included -- plus `be` (the biased exponent), `e` (the exponent of the
/// significand's low bit), `boundary` and `even`.
///
/// The route through memory is not a detour. `i64.shr_u` and the other 64-bit
/// bit operations are not in this compiler's IR, and eight `i32.load8_u` are.
/// wasm's linear memory is little-endian by definition, so the byte order is a
/// fact rather than a host property.
#[allow(clippy::too_many_arguments)]
fn f64_fields(
    b: &mut Vec<Ins>,
    x: u32,
    sc: u32,
    m: [u32; 4],
    be: u32,
    e: u32,
    boundary: u32,
    even: u32,
) {
    let byte = |b: &mut Vec<Ins>, k: u32| {
        b.push(ld(sc));
        b.push(Ins::I32Load8U(0, k));
    };
    b.push(ld(sc));
    b.extend_from_slice(&[ld(x), Ins::I64ReinterpretF64]);
    b.push(Ins::I64Store(ALIGN_WORD, 0));
    // m0 = b0 | b1 << 8, and so on up; m3 is the four bits b6 keeps.
    for k in 0..3u32 {
        byte(b, 2 * k);
        byte(b, 2 * k + 1);
        b.extend_from_slice(&[ic(8), Ins::I32Shl, Ins::I32Or, st(m[k as usize])]);
    }
    byte(b, 6);
    b.extend_from_slice(&[ic(15), Ins::I32And, st(m[3])]);
    // be = (b7 & 0x7F) * 16 + b6 / 16
    byte(b, 7);
    b.extend_from_slice(&[ic(0x7F), Ins::I32And, ic(16), Ins::I32Mul]);
    byte(b, 6);
    b.extend_from_slice(&[ic(16), Ins::I32DivS, Ins::I32Add, st(be)]);
    // even is bit 0 of the significand, which the implicit bit cannot reach.
    b.extend_from_slice(&[ld(m[0]), ic(1), Ins::I32And, Ins::I32Eqz, st(even)]);
    b.extend_from_slice(&[ic(0), st(boundary)]);
    b.extend_from_slice(&[ic(-1074), st(e)]);
    if_then(
        b,
        |b| b.push(ld(be)),
        |b| {
            // A normal: the gap below is half the gap above exactly when the
            // significand is the smallest one and the exponent is not the
            // smallest, which is 6.1.6.1.20's "closest" seen from below.
            b.extend_from_slice(&[
                ld(m[0]),
                ld(m[1]),
                Ins::I32Or,
                ld(m[2]),
                Ins::I32Or,
                ld(m[3]),
                Ins::I32Or,
                Ins::I32Eqz,
            ]);
            b.extend_from_slice(&[ic(1), ld(be), Ins::I32LtS, Ins::I32And, st(boundary)]);
            b.extend_from_slice(&[ld(m[3]), ic(16), Ins::I32Add, st(m[3])]);
            b.extend_from_slice(&[ld(be), ic(1075), Ins::I32Sub, st(e)]);
        },
    );
}

// -------------------------------------------------------------------------
// Number::toString
// -------------------------------------------------------------------------

/// `__dragon4(x, digits) -> (k, n)`, for a finite `x > 0`.
///
/// Writes `k` ASCII digits at `digits` and answers the pair
/// `0.d1d2...dk * 10^n` -- which is exactly the `s`, `k` and `n` of
/// ECMA-262 6.1.6.1.20 step 5, before its steps 6 to 9 lay them out.
fn dragon4(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2);
    let sc = f.local(ValType::I32);
    let m0 = f.local(ValType::I32);
    let m1 = f.local(ValType::I32);
    let m2 = f.local(ValType::I32);
    let m3 = f.local(ValType::I32);
    let be = f.local(ValType::I32);
    let e = f.local(ValType::I32);
    let boundary = f.local(ValType::I32);
    let even = f.local(ValType::I32);
    let r = f.local(ValType::I32);
    let s = f.local(ValType::I32);
    let mp = f.local(ValType::I32);
    let mm = f.local(ValType::I32);
    let t = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let k = f.local(ValType::I32);
    let d = f.local(ValType::I32);
    let dd = f.local(ValType::I32);
    let c = f.local(ValType::I32);
    let low = f.local(ValType::I32);
    let high = f.local(ValType::I32);
    let tb = f.local(ValType::I32);
    let stop = f.local(ValType::I32);
    let b = &mut f.body;

    b.extend_from_slice(&[ic(8), ctx.alloc(), st(sc)]);
    f64_fields(b, 0, sc, [m0, m1, m2, m3], be, e, boundary, even);

    for v in [r, s, mp, mm, t] {
        b.extend_from_slice(&[ic(D4_LIMBS), ctx.call(Cv::BnNew), st(v)]);
    }
    b.extend_from_slice(&[ld(r), ld(m0), ld(m1), ld(m2), ld(m3), ctx.call(Cv::BnSet4)]);

    let shl = |b: &mut Vec<Ins>, p: u32, amount: &[Ins]| {
        b.push(ld(p));
        b.extend_from_slice(amount);
        b.push(ctx.call(Cv::BnShl));
    };
    let seti = |b: &mut Vec<Ins>, p: u32, v: i32| {
        b.extend_from_slice(&[ld(p), ic(v), ctx.call(Cv::BnSeti)]);
    };

    // e >= 0
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(e), ic(0), Ins::I32LtS, Ins::I32Eqz]),
        |b| {
            if_then(
                b,
                |b| b.push(ld(boundary)),
                |b| {
                    shl(b, r, &[ld(e), ic(2), Ins::I32Add]);
                    seti(b, s, 4);
                    seti(b, mp, 1);
                    shl(b, mp, &[ld(e), ic(1), Ins::I32Add]);
                    seti(b, mm, 1);
                    shl(b, mm, &[ld(e)]);
                },
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(boundary), Ins::I32Eqz]),
                |b| {
                    shl(b, r, &[ld(e), ic(1), Ins::I32Add]);
                    seti(b, s, 2);
                    seti(b, mp, 1);
                    shl(b, mp, &[ld(e)]);
                    seti(b, mm, 1);
                    shl(b, mm, &[ld(e)]);
                },
            );
        },
    );
    // e < 0
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(e), ic(0), Ins::I32LtS]),
        |b| {
            if_then(
                b,
                |b| b.push(ld(boundary)),
                |b| {
                    shl(b, r, &[ic(2)]);
                    seti(b, s, 1);
                    shl(b, s, &[ic(2), ld(e), Ins::I32Sub]);
                    seti(b, mp, 2);
                    seti(b, mm, 1);
                },
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(boundary), Ins::I32Eqz]),
                |b| {
                    shl(b, r, &[ic(1)]);
                    seti(b, s, 1);
                    shl(b, s, &[ic(1), ld(e), Ins::I32Sub]);
                    seti(b, mp, 1);
                    seti(b, mm, 1);
                },
            );
        },
    );

    let mul10 = |b: &mut Vec<Ins>, p: u32| {
        b.extend_from_slice(&[ld(p), ic(10), ctx.call(Cv::BnMulSmall)]);
    };
    let cmp = |b: &mut Vec<Ins>, x: u32, y: u32| {
        b.extend_from_slice(&[ld(x), ld(y), ctx.call(Cv::BnCmp)]);
    };

    // ---- scale: settle `n` so that (r + m+)/s lands in (1/10, 1] --------
    //
    // The two tests are mirror strictnesses on purpose. If both were loose,
    // or both strict, they would overlap at (r + m+) * 10 == s and the loop
    // would multiply and divide forever; 1e23 reaches that point.
    // A seed, so the loop below is a fix-up and not the whole search.
    //
    // The loop is exact and would settle on its own, but for a subnormal it
    // would take 324 turns of multiplying a 1075-bit number by ten -- a real
    // cost in the guest, not just in the test. `bits(r) - bits(s)` is
    // log2(r/s) to within one, and 1233/4096 is log10(2) to seven places, so
    // one `10^n` puts the scale within a step or two of right. Nothing here
    // has to be correct: the loop that follows is still the authority.
    b.extend_from_slice(&[ld(r), ctx.call(Cv::BnBits)]);
    b.extend_from_slice(&[ld(s), ctx.call(Cv::BnBits), Ins::I32Sub]);
    b.extend_from_slice(&[ic(1233), Ins::I32Mul, ic(4096), Ins::I32DivS]);
    b.extend_from_slice(&[ic(1), Ins::I32Add, st(n)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ic(0), ld(n), Ins::I32LtS]),
        |b| b.extend_from_slice(&[ld(s), ld(n), ctx.call(Cv::BnMulPow10)]),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(n), ic(0), Ins::I32LtS]),
        |b| {
            for p in [r, mp, mm] {
                b.push(ld(p));
                b.extend_from_slice(&[ic(0), ld(n), Ins::I32Sub]);
                b.push(ctx.call(Cv::BnMulPow10));
            }
        },
    );
    b.extend_from_slice(&[ic(0), st(stop)]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
        |b| {
            b.extend_from_slice(&[ld(t), ld(r), ctx.call(Cv::BnCopy)]);
            b.extend_from_slice(&[ld(t), ld(mp), ctx.call(Cv::BnAdd)]);
            cmp(b, t, s);
            b.push(st(c));
            // too_big = even ? c >= 0 : c > 0
            b.extend_from_slice(&[ic(0), ld(c), Ins::I32LtS]);
            b.extend_from_slice(&[
                ld(even),
                ld(c),
                Ins::I32Eqz,
                Ins::I32And,
                Ins::I32Or,
                st(tb),
            ]);
            if_then(
                b,
                |b| b.push(ld(tb)),
                |b| {
                    mul10(b, s);
                    bump(b, n, 1);
                },
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(tb), Ins::I32Eqz]),
                |b| {
                    mul10(b, t);
                    cmp(b, t, s);
                    b.push(st(c));
                    // too_small = even ? c < 0 : c <= 0
                    b.extend_from_slice(&[ld(c), ic(0), Ins::I32LtS]);
                    b.extend_from_slice(&[
                        ld(even),
                        Ins::I32Eqz,
                        ld(c),
                        Ins::I32Eqz,
                        Ins::I32And,
                        Ins::I32Or,
                        st(tb),
                    ]);
                    if_then(
                        b,
                        |b| b.push(ld(tb)),
                        |b| {
                            mul10(b, r);
                            mul10(b, mp);
                            mul10(b, mm);
                            bump(b, n, -1);
                        },
                    );
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(tb), Ins::I32Eqz]),
                        |b| b.extend_from_slice(&[ic(1), st(stop)]),
                    );
                },
            );
        },
    );

    // ---- generate -------------------------------------------------------
    b.extend_from_slice(&[ic(0), st(k), ic(0), st(stop)]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
        |b| {
            mul10(b, r);
            mul10(b, mp);
            mul10(b, mm);
            // One decimal digit is at most nine subtractions, so the whole of
            // the division this algorithm needs is this loop.
            b.extend_from_slice(&[ic(0), st(d)]);
            while_loop(
                b,
                |b| {
                    cmp(b, r, s);
                    b.extend_from_slice(&[ic(0), Ins::I32LtS, Ins::I32Eqz]);
                },
                |b| {
                    b.extend_from_slice(&[ld(r), ld(s), ctx.call(Cv::BnSub)]);
                    bump(b, d, 1);
                },
            );
            cmp(b, r, mm);
            b.push(st(c));
            // low = even ? c <= 0 : c < 0
            b.extend_from_slice(&[ld(c), ic(0), Ins::I32LtS]);
            b.extend_from_slice(&[
                ld(even),
                ld(c),
                Ins::I32Eqz,
                Ins::I32And,
                Ins::I32Or,
                st(low),
            ]);
            b.extend_from_slice(&[ld(t), ld(r), ctx.call(Cv::BnCopy)]);
            b.extend_from_slice(&[ld(t), ld(mp), ctx.call(Cv::BnAdd)]);
            cmp(b, t, s);
            b.push(st(c));
            // high = even ? c >= 0 : c > 0
            b.extend_from_slice(&[ic(0), ld(c), Ins::I32LtS]);
            b.extend_from_slice(&[
                ld(even),
                ld(c),
                Ins::I32Eqz,
                Ins::I32And,
                Ins::I32Or,
                st(high),
            ]);
            b.extend_from_slice(&[ld(d), st(dd)]);
            b.extend_from_slice(&[ld(low), ld(high), Ins::I32Or, st(stop)]);
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(low), Ins::I32Eqz, ld(high), Ins::I32And]),
                |b| b.extend_from_slice(&[ld(d), ic(1), Ins::I32Add, st(dd)]),
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(low), ld(high), Ins::I32And]),
                |b| {
                    // Both neighbours are in reach: take the closer one, and
                    // on an exact tie the even `s`, which is the last sentence
                    // of step 5 and the one Rust's formatter does not follow.
                    b.extend_from_slice(&[ld(t), ld(r), ctx.call(Cv::BnCopy)]);
                    b.extend_from_slice(&[ld(t), ld(r), ctx.call(Cv::BnAdd)]);
                    cmp(b, t, s);
                    b.push(st(c));
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ic(0), ld(c), Ins::I32LtS]),
                        |b| b.extend_from_slice(&[ld(d), ic(1), Ins::I32Add, st(dd)]),
                    );
                    if_then(
                        b,
                        |b| {
                            b.extend_from_slice(&[ld(c), Ins::I32Eqz]);
                            b.extend_from_slice(&[ld(d), ic(1), Ins::I32And, Ins::I32And]);
                        },
                        |b| b.extend_from_slice(&[ld(d), ic(1), Ins::I32Add, st(dd)]),
                    );
                },
            );
            b.extend_from_slice(&[ld(1), ld(k), Ins::I32Add]);
            b.extend_from_slice(&[ld(dd), ic(48), Ins::I32Add]);
            b.push(Ins::I32Store8(0, 0));
            bump(b, k, 1);
        },
    );

    b.push(ld(k));
    b.push(ld(n));
    f
}

/// `__num_to_string(x) -> string pointer`: ECMA-262 6.1.6.1.20 whole, radix
/// ten. Steps 1 to 4 are the four interned literals; step 5 is
/// [`Cv::Dragon4`]; steps 6 to 9 are the layout below, and the three
/// thresholds in them -- `n <= 21`, `-6 < n`, and everything else in
/// exponential form -- are the spec's own numbers rather than any formatter's.
fn num_to_string(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let x = f.local(ValType::F64);
    let neg = f.local(ValType::I32);
    let digits = f.local(ValType::I32);
    let p = f.local(ValType::I32);
    let w = f.local(ValType::I32);
    let k = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let ee = f.local(ValType::I32);
    let wide = f.local(ValType::I32);
    let b = &mut f.body;

    // Step 1.
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(0), ld(0), Ins::F64Ne]),
        |b| {
            b.push(ic(ctx.names.nan));
            b.push(Ins::Return);
        },
    );
    // Step 2 -- and `-0.0 == 0.0`, which is why -0 answers "0" here.
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(0), Ins::F64Const(0.0), Ins::F64Eq]),
        |b| {
            b.push(ic(ctx.names.zero));
            b.push(Ins::Return);
        },
    );
    // Step 3.
    b.extend_from_slice(&[ld(0), Ins::F64Const(0.0), Ins::F64Lt, st(neg)]);
    b.extend_from_slice(&[ld(0), Ins::F64Abs, st(x)]);
    // Step 4.
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ld(x), Ins::F64Const(f64::INFINITY), Ins::F64Eq]);
        },
        |b| {
            if_then(
                b,
                |b| b.push(ld(neg)),
                |b| {
                    b.push(ic(ctx.names.neg_infinity));
                    b.push(Ins::Return);
                },
            );
            b.push(ic(ctx.names.infinity));
            b.push(Ins::Return);
        },
    );

    // Fast path, ahead of Dragon4: an integer below 2^31 prints as its
    // digits (steps 6-7 with k == n, no exponent, no point), and the general
    // path cost ~5 200 steps for `"" + n` -- the single most expensive thing
    // a migrated script did per line. Measured through the CLI at 1012da1.
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ld(x), Ins::F64Const(2_147_483_648.0), Ins::F64Lt]);
            b.extend_from_slice(&[ld(x), ld(x), Ins::F64Trunc, Ins::F64Eq, Ins::I32And]);
        },
        |b| {
            b.extend_from_slice(&[ld(x), Ins::I32TruncF64S, st(n)]);
            b.extend_from_slice(&[ic(STRING_HEADER + 12), ctx.alloc(), st(p)]);
            b.extend_from_slice(&[ic(0), st(w)]);
            if_then(
                b,
                |b| b.push(ld(neg)),
                |b| {
                    b.extend_from_slice(&[ld(p), ld(w), Ins::I32Add, ic(45)]);
                    b.push(Ins::I32Store8(0, STRING_HEADER as u32));
                    bump(b, w, 1);
                },
            );
            // How many digits: at least one, so 0 prints as "0".
            b.extend_from_slice(&[ld(n), st(k), ic(0), st(i)]);
            while_loop(
                b,
                |b| b.extend_from_slice(&[ld(k), ic(0), Ins::I32Ne, ld(i), Ins::I32Eqz, Ins::I32Or]),
                |b| {
                    bump(b, i, 1);
                    b.extend_from_slice(&[ld(k), ic(10), Ins::I32DivS, st(k)]);
                },
            );
            // Written from the end, least significant digit first.
            b.extend_from_slice(&[ld(w), ld(i), Ins::I32Add, st(w), ld(w), st(k)]);
            while_loop(
                b,
                |b| b.extend_from_slice(&[ld(i), ic(0), Ins::I32Ne]),
                |b| {
                    bump(b, k, -1);
                    bump(b, i, -1);
                    b.extend_from_slice(&[ld(p), ld(k), Ins::I32Add]);
                    b.extend_from_slice(&[ld(n), ic(10), Ins::I32RemS, ic(48), Ins::I32Add]);
                    b.push(Ins::I32Store8(0, STRING_HEADER as u32));
                    b.extend_from_slice(&[ld(n), ic(10), Ins::I32DivS, st(n)]);
                },
            );
            b.extend_from_slice(&[ld(p), ld(w), Ins::I32Store(ALIGN_WORD, 0), ld(p), Ins::Return]);
        },
    );

    b.extend_from_slice(&[ic(24), ctx.alloc(), st(digits)]);
    b.extend_from_slice(&[ld(x), ld(digits), ctx.call(Cv::Dragon4)]);
    b.extend_from_slice(&[st(n), st(k)]);

    // The record is allocated at its widest and its length header written
    // last. The widest is "-0." + six zeros + seventeen digits, which is 27,
    // and "-1.2345678901234567e-308", which is 24.
    b.extend_from_slice(&[ic(STRING_HEADER + 32), ctx.alloc(), st(p)]);
    b.extend_from_slice(&[ic(0), st(w)]);

    // `put` appends one byte, whose value the closure leaves on the stack.
    let put = |b: &mut Vec<Ins>, value: &[Ins]| {
        b.extend_from_slice(&[ld(p), ld(w), Ins::I32Add]);
        b.extend_from_slice(value);
        b.push(Ins::I32Store8(0, STRING_HEADER as u32));
        bump(b, w, 1);
    };
    let put_digit = |b: &mut Vec<Ins>, idx: u32| {
        b.extend_from_slice(&[ld(p), ld(w), Ins::I32Add]);
        b.extend_from_slice(&[ld(digits), ld(idx), Ins::I32Add]);
        b.push(Ins::I32Load8U(0, 0));
        b.push(Ins::I32Store8(0, STRING_HEADER as u32));
        bump(b, w, 1);
    };

    if_then(b, |b| b.push(ld(neg)), |b| put(b, &[ic(45)]));

    // Step 6: k <= n <= 21.
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ld(n), ld(k), Ins::I32LtS, Ins::I32Eqz]);
            b.extend_from_slice(&[ic(21), ld(n), Ins::I32LtS, Ins::I32Eqz, Ins::I32And]);
        },
        |b| {
            b.extend_from_slice(&[ic(0), st(i)]);
            while_loop(
                b,
                |b| lt(b, i, k),
                |b| {
                    put_digit(b, i);
                    bump(b, i, 1);
                },
            );
            while_loop(
                b,
                |b| lt(b, i, n),
                |b| {
                    put(b, &[ic(48)]);
                    bump(b, i, 1);
                },
            );
            b.push(ld(p));
            b.push(ld(w));
            b.push(Ins::I32Store(ALIGN_WORD, 0));
            b.push(ld(p));
            b.push(Ins::Return);
        },
    );
    // Step 7: 0 < n <= 21.
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ic(0), ld(n), Ins::I32LtS]);
            b.extend_from_slice(&[ic(21), ld(n), Ins::I32LtS, Ins::I32Eqz, Ins::I32And]);
        },
        |b| {
            b.extend_from_slice(&[ic(0), st(i)]);
            while_loop(
                b,
                |b| lt(b, i, n),
                |b| {
                    put_digit(b, i);
                    bump(b, i, 1);
                },
            );
            put(b, &[ic(46)]);
            while_loop(
                b,
                |b| lt(b, i, k),
                |b| {
                    put_digit(b, i);
                    bump(b, i, 1);
                },
            );
            b.push(ld(p));
            b.push(ld(w));
            b.push(Ins::I32Store(ALIGN_WORD, 0));
            b.push(ld(p));
            b.push(Ins::Return);
        },
    );
    // Step 8: -6 < n <= 0.
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ic(-6), ld(n), Ins::I32LtS]);
            b.extend_from_slice(&[ld(n), ic(1), Ins::I32LtS, Ins::I32And]);
        },
        |b| {
            put(b, &[ic(48)]);
            put(b, &[ic(46)]);
            b.extend_from_slice(&[ic(0), st(i)]);
            while_loop(
                b,
                |b| b.extend_from_slice(&[ld(i), ic(0), ld(n), Ins::I32Sub, Ins::I32LtS]),
                |b| {
                    put(b, &[ic(48)]);
                    bump(b, i, 1);
                },
            );
            b.extend_from_slice(&[ic(0), st(i)]);
            while_loop(
                b,
                |b| lt(b, i, k),
                |b| {
                    put_digit(b, i);
                    bump(b, i, 1);
                },
            );
            b.push(ld(p));
            b.push(ld(w));
            b.push(Ins::I32Store(ALIGN_WORD, 0));
            b.push(ld(p));
            b.push(Ins::Return);
        },
    );
    // Step 9: exponential form.
    b.extend_from_slice(&[ic(0), st(i)]);
    put_digit(b, i);
    if_then(
        b,
        |b| b.extend_from_slice(&[ic(1), ld(k), Ins::I32LtS]),
        |b| {
            put(b, &[ic(46)]);
            b.extend_from_slice(&[ic(1), st(i)]);
            while_loop(
                b,
                |b| lt(b, i, k),
                |b| {
                    put_digit(b, i);
                    bump(b, i, 1);
                },
            );
        },
    );
    put(b, &[ic(101)]);
    b.extend_from_slice(&[ld(n), ic(1), Ins::I32Sub, st(ee)]);
    // The sign is decided before the magnitude is taken, or the negation
    // would make the second test fire as well and write "e-+7".
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(ee), ic(0), Ins::I32LtS, Ins::I32Eqz]),
        |b| put(b, &[ic(43)]),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(ee), ic(0), Ins::I32LtS]),
        |b| {
            put(b, &[ic(45)]);
            b.extend_from_slice(&[ic(0), ld(ee), Ins::I32Sub, st(ee)]);
        },
    );
    // Once a higher digit has been written every lower one follows, zero or
    // not: 308 is "308" and not "38". `wide` is what carries that.
    b.extend_from_slice(&[ic(0), st(wide)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ic(99), ld(ee), Ins::I32LtS]),
        |b| {
            put(b, &[ld(ee), ic(100), Ins::I32DivS, ic(48), Ins::I32Add]);
            b.extend_from_slice(&[ld(ee), ic(100), Ins::I32RemS, st(ee)]);
            b.extend_from_slice(&[ic(1), st(wide)]);
        },
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ic(9), ld(ee), Ins::I32LtS, ld(wide), Ins::I32Or]),
        |b| {
            put(b, &[ld(ee), ic(10), Ins::I32DivS, ic(48), Ins::I32Add]);
            b.extend_from_slice(&[ld(ee), ic(10), Ins::I32RemS, st(ee)]);
        },
    );
    put(b, &[ld(ee), ic(48), Ins::I32Add]);
    b.push(ld(p));
    b.push(ld(w));
    b.push(Ins::I32Store(ALIGN_WORD, 0));
    b.push(ld(p));
    f
}

// -------------------------------------------------------------------------
// StringToNumber
// -------------------------------------------------------------------------

/// Push the byte at index `idx` (an already-built `i32` run) of the string
/// record in local `s`.
fn str_byte(b: &mut Vec<Ins>, s: u32, idx: &[Ins]) {
    b.push(ld(s));
    b.extend_from_slice(idx);
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, STRING_HEADER as u32));
}

/// `__ws_len(s, i, len) -> the byte length of the StrWhiteSpace at i, or 0`.
///
/// ECMA-262 7.1.4.1 takes StrWhiteSpace to be WhiteSpace or LineTerminator,
/// which is not the ASCII six: it is also NBSP, ZWNBSP, LS, PS and the Zs
/// category. The strings here are UTF-8, so those are byte sequences and this
/// matches them as such rather than decoding a code point it would only
/// compare against a table.
fn ws_len() -> FnBuild {
    let mut f = FnBuild::new(3);
    let c = f.local(ValType::I32);
    let b1 = f.local(ValType::I32);
    let b2 = f.local(ValType::I32);
    let b = &mut f.body;
    str_byte(b, 0, &[ld(1)]);
    b.push(st(c));
    // TAB, LF, VT, FF, CR and SPACE.
    let ret = |b: &mut Vec<Ins>, v: i32| {
        b.push(ic(v));
        b.push(Ins::Return);
    };
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ld(c), ic(9), Ins::I32LtS, Ins::I32Eqz]);
            b.extend_from_slice(&[ic(13), ld(c), Ins::I32LtS, Ins::I32Eqz, Ins::I32And]);
            b.extend_from_slice(&[ld(c), ic(32), Ins::I32Eq, Ins::I32Or]);
        },
        |b| ret(b, 1),
    );
    // U+00A0 -- two bytes.
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ld(c), ic(0xC2), Ins::I32Eq]);
            b.extend_from_slice(&[ld(1), ic(1), Ins::I32Add, ld(2), Ins::I32LtS, Ins::I32And]);
        },
        |b| {
            if_then(
                b,
                |b| {
                    str_byte(b, 0, &[ld(1), ic(1), Ins::I32Add]);
                    b.extend_from_slice(&[ic(0xA0), Ins::I32Eq]);
                },
                |b| ret(b, 2),
            );
        },
    );
    // Everything else is three bytes.
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ld(1), ic(2), Ins::I32Add, ld(2), Ins::I32LtS]);
        },
        |b| {
            str_byte(b, 0, &[ld(1), ic(1), Ins::I32Add]);
            b.push(st(b1));
            str_byte(b, 0, &[ld(1), ic(2), Ins::I32Add]);
            b.push(st(b2));
            // U+1680.
            if_then(
                b,
                |b| {
                    b.extend_from_slice(&[ld(c), ic(0xE1), Ins::I32Eq]);
                    b.extend_from_slice(&[ld(b1), ic(0x9A), Ins::I32Eq, Ins::I32And]);
                    b.extend_from_slice(&[ld(b2), ic(0x80), Ins::I32Eq, Ins::I32And]);
                },
                |b| ret(b, 3),
            );
            // U+2000..U+200A, U+2028, U+2029, U+202F, U+205F.
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(c), ic(0xE2), Ins::I32Eq]),
                |b| {
                    if_then(
                        b,
                        |b| {
                            b.extend_from_slice(&[ld(b1), ic(0x80), Ins::I32Eq]);
                            b.extend_from_slice(&[ld(b2), ic(0x80), Ins::I32LtS, Ins::I32Eqz]);
                            b.extend_from_slice(&[ic(0x8A), ld(b2), Ins::I32LtS, Ins::I32Eqz]);
                            b.extend_from_slice(&[Ins::I32And, Ins::I32And]);
                            b.extend_from_slice(&[ld(b1), ic(0x80), Ins::I32Eq]);
                            b.extend_from_slice(&[ld(b2), ic(0xA8), Ins::I32Eq, Ins::I32And]);
                            b.push(Ins::I32Or);
                            b.extend_from_slice(&[ld(b1), ic(0x80), Ins::I32Eq]);
                            b.extend_from_slice(&[ld(b2), ic(0xA9), Ins::I32Eq, Ins::I32And]);
                            b.push(Ins::I32Or);
                            b.extend_from_slice(&[ld(b1), ic(0x80), Ins::I32Eq]);
                            b.extend_from_slice(&[ld(b2), ic(0xAF), Ins::I32Eq, Ins::I32And]);
                            b.push(Ins::I32Or);
                            b.extend_from_slice(&[ld(b1), ic(0x81), Ins::I32Eq]);
                            b.extend_from_slice(&[ld(b2), ic(0x9F), Ins::I32Eq, Ins::I32And]);
                            b.push(Ins::I32Or);
                        },
                        |b| ret(b, 3),
                    );
                },
            );
            // U+3000.
            if_then(
                b,
                |b| {
                    b.extend_from_slice(&[ld(c), ic(0xE3), Ins::I32Eq]);
                    b.extend_from_slice(&[ld(b1), ic(0x80), Ins::I32Eq, Ins::I32And]);
                    b.extend_from_slice(&[ld(b2), ic(0x80), Ins::I32Eq, Ins::I32And]);
                },
                |b| ret(b, 3),
            );
            // U+FEFF.
            if_then(
                b,
                |b| {
                    b.extend_from_slice(&[ld(c), ic(0xEF), Ins::I32Eq]);
                    b.extend_from_slice(&[ld(b1), ic(0xBB), Ins::I32Eq, Ins::I32And]);
                    b.extend_from_slice(&[ld(b2), ic(0xBF), Ins::I32Eq, Ins::I32And]);
                },
                |b| ret(b, 3),
            );
        },
    );
    b.push(ic(0));
    f
}

/// `__skip_ws(s, i, len) -> the first index at or after i that is not
/// StrWhiteSpace`.
fn skip_ws(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(3);
    let i = f.local(ValType::I32);
    let w = f.local(ValType::I32);
    let stop = f.local(ValType::I32);
    let b = &mut f.body;
    b.extend_from_slice(&[ld(1), st(i), ic(0), st(stop)]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
        |b| {
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(i), ld(2), Ins::I32LtS, Ins::I32Eqz]),
                |b| b.extend_from_slice(&[ic(1), st(stop)]),
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                |b| {
                    b.extend_from_slice(&[ld(0), ld(i), ld(2), ctx.call(Cv::WsLen), st(w)]);
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(w), Ins::I32Eqz]),
                        |b| b.extend_from_slice(&[ic(1), st(stop)]),
                    );
                    if_then(
                        b,
                        |b| b.push(ld(w)),
                        |b| b.extend_from_slice(&[ld(i), ld(w), Ins::I32Add, st(i)]),
                    );
                },
            );
        },
    );
    b.push(ld(i));
    f
}

/// `__ratio_to_f64(n, d, scratch, sticky) -> the correctly rounded binary64
/// nearest to n/d`, with `sticky` saying that the true value is a shade above
/// `n/d` because digits were dropped.
///
/// One division and one rounding, which is what "correctly rounded" means: an
/// accumulated product of roundings is not the same number, and `"0.1"` is
/// where the difference shows.
///
/// `n` and `d` are both consumed -- `n` ends as the remainder, `d` as its
/// shifted self -- because this is the last thing either is used for.
fn ratio_to_f64(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(4);
    let bn = f.local(ValType::I32);
    let bd = f.local(ValType::I32);
    let te = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let q0 = f.local(ValType::I32);
    let q1 = f.local(ValType::I32);
    let q2 = f.local(ValType::I32);
    let q3 = f.local(ValType::I32);
    let g = f.local(ValType::I32);
    let stk = f.local(ValType::I32);
    let shifts = f.local(ValType::I32);
    let lo = f.local(ValType::I32);
    let c = f.local(ValType::I32);
    let up = f.local(ValType::I32);
    let biased = f.local(ValType::I32);
    let sc = f.local(ValType::I32);
    let b = &mut f.body;

    let shr1 = |b: &mut Vec<Ins>| {
        b.extend_from_slice(&[ld(q0), ic(2), Ins::I32DivS]);
        b.extend_from_slice(&[ld(q1), ic(1), Ins::I32And, ic(15), Ins::I32Shl]);
        b.extend_from_slice(&[Ins::I32Or, st(q0)]);
        b.extend_from_slice(&[ld(q1), ic(2), Ins::I32DivS]);
        b.extend_from_slice(&[ld(q2), ic(1), Ins::I32And, ic(15), Ins::I32Shl]);
        b.extend_from_slice(&[Ins::I32Or, st(q1)]);
        b.extend_from_slice(&[ld(q2), ic(2), Ins::I32DivS]);
        b.extend_from_slice(&[ld(q3), ic(1), Ins::I32And, ic(15), Ins::I32Shl]);
        b.extend_from_slice(&[Ins::I32Or, st(q2)]);
        b.extend_from_slice(&[ld(q3), ic(2), Ins::I32DivS, st(q3)]);
    };

    b.extend_from_slice(&[ld(0), ctx.call(Cv::BnBits), st(bn)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(bn), Ins::I32Eqz]),
        |b| {
            b.push(Ins::F64Const(0.0));
            b.push(Ins::Return);
        },
    );
    b.extend_from_slice(&[ld(1), ctx.call(Cv::BnBits), st(bd)]);
    // The binary exponent of the result's low bit, clamped at the subnormal
    // floor so that one code path covers normals and subnormals both.
    b.extend_from_slice(&[ld(bn), ld(bd), Ins::I32Sub, ic(54), Ins::I32Sub, st(te)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(te), ic(-1074), Ins::I32LtS]),
        |b| b.extend_from_slice(&[ic(-1074), st(te)]),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(te), ic(0), Ins::I32LtS, Ins::I32Eqz]),
        |b| b.extend_from_slice(&[ld(1), ld(te), ctx.call(Cv::BnShl)]),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(te), ic(0), Ins::I32LtS]),
        |b| b.extend_from_slice(&[ld(0), ic(0), ld(te), Ins::I32Sub, ctx.call(Cv::BnShl)]),
    );
    // Fifty-six shift-and-subtract steps against a divisor walking down: the
    // quotient is at most 55 bits, so a general multi-limb division would be
    // machinery with nothing to do.
    b.extend_from_slice(&[ld(2), ld(1), ctx.call(Cv::BnCopy)]);
    b.extend_from_slice(&[ld(2), ic(55), ctx.call(Cv::BnShl)]);
    for q in [q0, q1, q2, q3] {
        b.extend_from_slice(&[ic(0), st(q)]);
    }
    b.extend_from_slice(&[ic(0), st(i)]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ld(i), ic(56), Ins::I32LtS]),
        |b| {
            b.extend_from_slice(&[ld(q3), ic(1), Ins::I32Shl]);
            b.extend_from_slice(&[ld(q2), ic(32768), Ins::I32DivS, Ins::I32Or]);
            b.extend_from_slice(&[ic(0xFFFF), Ins::I32And, st(q3)]);
            b.extend_from_slice(&[ld(q2), ic(1), Ins::I32Shl]);
            b.extend_from_slice(&[ld(q1), ic(32768), Ins::I32DivS, Ins::I32Or]);
            b.extend_from_slice(&[ic(0xFFFF), Ins::I32And, st(q2)]);
            b.extend_from_slice(&[ld(q1), ic(1), Ins::I32Shl]);
            b.extend_from_slice(&[ld(q0), ic(32768), Ins::I32DivS, Ins::I32Or]);
            b.extend_from_slice(&[ic(0xFFFF), Ins::I32And, st(q1)]);
            b.extend_from_slice(&[ld(q0), ic(1), Ins::I32Shl, ic(0xFFFF), Ins::I32And, st(q0)]);
            if_then(
                b,
                |b| {
                    b.extend_from_slice(&[ld(0), ld(2), ctx.call(Cv::BnCmp)]);
                    b.extend_from_slice(&[ic(0), Ins::I32LtS, Ins::I32Eqz]);
                },
                |b| {
                    b.extend_from_slice(&[ld(0), ld(2), ctx.call(Cv::BnSub)]);
                    b.extend_from_slice(&[ld(q0), ic(1), Ins::I32Or, st(q0)]);
                },
            );
            b.extend_from_slice(&[ld(2), ctx.call(Cv::BnShr1)]);
            bump(b, i, 1);
        },
    );

    // Everything below the quotient's low bit, as one "is it non-zero".
    b.extend_from_slice(&[ld(0), ctx.call(Cv::BnBits)]);
    b.extend_from_slice(&[ic(0), Ins::I32Ne, ld(3), Ins::I32Or, st(lo)]);
    b.extend_from_slice(&[ic(0), st(g), ic(0), st(stk), ic(0), st(shifts)]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ic(31), ld(q3), Ins::I32LtS]),
        |b| {
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(shifts), Ins::I32Eqz]),
                |b| b.extend_from_slice(&[ld(lo), st(stk)]),
            );
            if_then(
                b,
                |b| b.push(ld(shifts)),
                |b| b.extend_from_slice(&[ld(stk), ld(g), Ins::I32Or, st(stk)]),
            );
            b.extend_from_slice(&[ld(q0), ic(1), Ins::I32And, st(g)]);
            shr1(b);
            bump(b, te, 1);
            bump(b, shifts, 1);
        },
    );
    b.extend_from_slice(&[ic(0), st(up)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(shifts), Ins::I32Eqz]),
        |b| {
            // No shift happened, so the residue is still a fraction of the
            // divisor: compare twice the remainder against it.
            b.extend_from_slice(&[ld(2), ld(0), ctx.call(Cv::BnCopy)]);
            b.extend_from_slice(&[ld(2), ld(0), ctx.call(Cv::BnAdd)]);
            b.extend_from_slice(&[ld(2), ld(1), ctx.call(Cv::BnCmp), st(c)]);
            b.extend_from_slice(&[ic(0), ld(c), Ins::I32LtS]);
            b.extend_from_slice(&[ld(c), Ins::I32Eqz]);
            b.extend_from_slice(&[ld(3), ld(q0), ic(1), Ins::I32And, Ins::I32Or, Ins::I32And]);
            b.extend_from_slice(&[Ins::I32Or, st(up)]);
        },
    );
    if_then(
        b,
        |b| b.push(ld(shifts)),
        |b| {
            b.extend_from_slice(&[ld(stk), ld(q0), ic(1), Ins::I32And, Ins::I32Or]);
            b.extend_from_slice(&[ld(g), Ins::I32And, st(up)]);
        },
    );
    if_then(
        b,
        |b| b.push(ld(up)),
        |b| {
            b.extend_from_slice(&[ld(q0), ic(1), Ins::I32Add, st(q0)]);
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(q0), ic(65536), Ins::I32Eq]),
                |b| {
                    b.extend_from_slice(&[ic(0), st(q0), ld(q1), ic(1), Ins::I32Add, st(q1)]);
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(q1), ic(65536), Ins::I32Eq]),
                        |b| {
                            b.extend_from_slice(&[
                                ic(0),
                                st(q1),
                                ld(q2),
                                ic(1),
                                Ins::I32Add,
                                st(q2),
                            ]);
                            if_then(
                                b,
                                |b| b.extend_from_slice(&[ld(q2), ic(65536), Ins::I32Eq]),
                                |b| {
                                    b.extend_from_slice(&[ic(0), st(q2)]);
                                    b.extend_from_slice(&[ld(q3), ic(1), Ins::I32Add, st(q3)]);
                                },
                            );
                        },
                    );
                },
            );
            // A carry out of 53 bits is the one case where rounding changes
            // the exponent.
            if_then(
                b,
                |b| b.extend_from_slice(&[ic(31), ld(q3), Ins::I32LtS]),
                |b| {
                    shr1(b);
                    bump(b, te, 1);
                },
            );
        },
    );

    b.extend_from_slice(&[ld(te), ic(1075), Ins::I32Add, st(biased)]);
    b.extend_from_slice(&[ld(q3), ic(16), Ins::I32Sub, st(q3)]);
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ld(te), ic(-1074), Ins::I32Eq]);
            b.extend_from_slice(&[ld(q3), ic(0), Ins::I32LtS, Ins::I32And]);
        },
        |b| {
            b.extend_from_slice(&[ic(0), st(biased)]);
            b.extend_from_slice(&[ld(q3), ic(16), Ins::I32Add, st(q3)]);
        },
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ic(2046), ld(biased), Ins::I32LtS]),
        |b| {
            b.push(Ins::F64Const(f64::INFINITY));
            b.push(Ins::Return);
        },
    );
    b.extend_from_slice(&[ic(8), ctx.alloc(), st(sc)]);
    let byte = |b: &mut Vec<Ins>, off: u32, value: &[Ins]| {
        b.push(ld(sc));
        b.extend_from_slice(value);
        b.push(Ins::I32Store8(0, off));
    };
    byte(b, 0, &[ld(q0), ic(255), Ins::I32And]);
    byte(b, 1, &[ld(q0), ic(256), Ins::I32DivS]);
    byte(b, 2, &[ld(q1), ic(255), Ins::I32And]);
    byte(b, 3, &[ld(q1), ic(256), Ins::I32DivS]);
    byte(b, 4, &[ld(q2), ic(255), Ins::I32And]);
    byte(b, 5, &[ld(q2), ic(256), Ins::I32DivS]);
    byte(
        b,
        6,
        &[
            ld(q3),
            ic(15),
            Ins::I32And,
            ld(biased),
            ic(15),
            Ins::I32And,
            ic(4),
            Ins::I32Shl,
            Ins::I32Or,
        ],
    );
    byte(b, 7, &[ld(biased), ic(16), Ins::I32DivS]);
    b.push(ld(sc));
    b.push(Ins::I64Load(ALIGN_WORD, 0));
    b.push(Ins::F64ReinterpretI64);
    f
}

/// `__digits_to_f64(digits, nd, e10, sticky) -> |value|`, where `digits` holds
/// `nd` values in `0 ..= 9` and the value is that integer times `10^e10`.
fn digits_to_f64(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(4);
    let i = f.local(ValType::I32);
    let j = f.local(ValType::I32);
    let v = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let d = f.local(ValType::F64);
    let pw = f.local(ValType::F64);
    let b = &mut f.body;

    // Clinger's exact case: an exact significand and an exact power of ten
    // means the single operation below is the only rounding, so it is the
    // correctly rounded one. Fifteen digits keeps the significand under 2^53
    // and 22 is the largest power of ten a binary64 holds exactly.
    if_then(
        b,
        |b| {
            b.extend_from_slice(&[ic(15), ld(1), Ins::I32LtS, Ins::I32Eqz]);
            b.extend_from_slice(&[ic(22), ld(2), Ins::I32LtS, Ins::I32Eqz, Ins::I32And]);
            b.extend_from_slice(&[ld(2), ic(-22), Ins::I32LtS, Ins::I32Eqz, Ins::I32And]);
            b.extend_from_slice(&[ld(3), Ins::I32Eqz, Ins::I32And]);
        },
        |b| {
            b.extend_from_slice(&[Ins::F64Const(0.0), st(d), ic(0), st(i)]);
            while_loop(
                b,
                |b| lt(b, i, 1),
                |b| {
                    b.extend_from_slice(&[ld(d), Ins::F64Const(10.0), Ins::F64Mul]);
                    b.extend_from_slice(&[ld(0), ld(i), Ins::I32Add]);
                    b.push(Ins::I32Load8U(0, 0));
                    b.extend_from_slice(&[Ins::F64ConvertI32S, Ins::F64Add, st(d)]);
                    bump(b, i, 1);
                },
            );
            b.extend_from_slice(&[Ins::F64Const(1.0), st(pw)]);
            b.extend_from_slice(&[ld(2), st(j)]);
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(j), ic(0), Ins::I32LtS]),
                |b| b.extend_from_slice(&[ic(0), ld(j), Ins::I32Sub, st(j)]),
            );
            while_loop(
                b,
                |b| b.push(ld(j)),
                |b| {
                    b.extend_from_slice(&[ld(pw), Ins::F64Const(10.0), Ins::F64Mul, st(pw)]);
                    bump(b, j, -1);
                },
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(2), ic(0), Ins::I32LtS]),
                |b| {
                    b.extend_from_slice(&[ld(d), ld(pw), Ins::F64Div]);
                    b.push(Ins::Return);
                },
            );
            b.extend_from_slice(&[ld(d), ld(pw), Ins::F64Mul]);
            b.push(Ins::Return);
        },
    );

    // The exact path: build the significand, then divide once.
    let n0 = f.local(ValType::I32);
    let dv = f.local(ValType::I32);
    let ts = f.local(ValType::I32);
    let b = &mut f.body;
    for p in [n0, dv, ts] {
        b.extend_from_slice(&[ic(S2N_LIMBS), ctx.call(Cv::BnNew), st(p)]);
    }
    b.extend_from_slice(&[ld(n0), ic(0), ctx.call(Cv::BnSeti)]);
    b.extend_from_slice(&[ic(0), st(i)]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ld(1), ld(i), ic(4), Ins::I32Add, Ins::I32LtS, Ins::I32Eqz]),
        |b| {
            b.extend_from_slice(&[ic(0), st(v), ic(0), st(n)]);
            while_loop(
                b,
                |b| b.extend_from_slice(&[ld(n), ic(4), Ins::I32LtS]),
                |b| {
                    b.extend_from_slice(&[ld(v), ic(10), Ins::I32Mul]);
                    b.extend_from_slice(&[ld(0), ld(i), Ins::I32Add, ld(n), Ins::I32Add]);
                    b.push(Ins::I32Load8U(0, 0));
                    b.extend_from_slice(&[Ins::I32Add, st(v)]);
                    bump(b, n, 1);
                },
            );
            b.extend_from_slice(&[ld(n0), ic(10000), ctx.call(Cv::BnMulSmall)]);
            b.extend_from_slice(&[ld(n0), ld(v), ctx.call(Cv::BnAddSmall)]);
            bump(b, i, 4);
        },
    );
    while_loop(
        b,
        |b| lt(b, i, 1),
        |b| {
            b.extend_from_slice(&[ld(n0), ic(10), ctx.call(Cv::BnMulSmall)]);
            b.push(ld(n0));
            b.extend_from_slice(&[ld(0), ld(i), Ins::I32Add]);
            b.push(Ins::I32Load8U(0, 0));
            b.push(ctx.call(Cv::BnAddSmall));
            bump(b, i, 1);
        },
    );
    b.extend_from_slice(&[ld(dv), ic(1), ctx.call(Cv::BnSeti)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(2), ic(0), Ins::I32LtS, Ins::I32Eqz]),
        |b| b.extend_from_slice(&[ld(n0), ld(2), ctx.call(Cv::BnMulPow10)]),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(2), ic(0), Ins::I32LtS]),
        |b| {
            b.push(ld(dv));
            b.extend_from_slice(&[ic(0), ld(2), Ins::I32Sub]);
            b.push(ctx.call(Cv::BnMulPow10));
        },
    );
    b.extend_from_slice(&[ld(n0), ld(dv), ld(ts), ld(3), ctx.call(Cv::RatioToF64)]);
    f
}

/// `__str_to_num(s) -> f64`: ECMA-262 7.1.4.1 over the whole
/// `StringNumericLiteral` grammar. A string that is not one is `NaN`; an empty
/// or whitespace-only string is `+0`, which is the one place the grammar's
/// empty production is the answer rather than a failure.
fn str_to_num(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let len = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let j = f.local(ValType::I32);
    let c = f.local(ValType::I32);
    let neg = f.local(ValType::I32);
    let radix = f.local(ValType::I32);
    let dv = f.local(ValType::I32);
    let cnt = f.local(ValType::I32);
    let over = f.local(ValType::I32);
    let stop = f.local(ValType::I32);
    let nbig = f.local(ValType::I32);
    let dbig = f.local(ValType::I32);
    let tbig = f.local(ValType::I32);
    let dbuf = f.local(ValType::I32);
    let gg = f.local(ValType::I32);
    let pp = f.local(ValType::I32);
    let lz = f.local(ValType::I32);
    let nd = f.local(ValType::I32);
    let seen = f.local(ValType::I32);
    let sticky = f.local(ValType::I32);
    let e10 = f.local(ValType::I32);
    let ev = f.local(ValType::I32);
    let esign = f.local(ValType::I32);
    let val = f.local(ValType::F64);
    let b = &mut f.body;

    let nan = |b: &mut Vec<Ins>| {
        b.push(Ins::F64Const(f64::NAN));
        b.push(Ins::Return);
    };
    let signed = |b: &mut Vec<Ins>, x: f64| {
        if_then(
            b,
            |b| b.push(ld(neg)),
            |b| {
                b.push(Ins::F64Const(x));
                b.push(Ins::F64Neg);
                b.push(Ins::Return);
            },
        );
        b.push(Ins::F64Const(x));
        b.push(Ins::Return);
    };

    b.push(ld(0));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(st(len));
    b.extend_from_slice(&[ic(0), st(neg)]);
    b.extend_from_slice(&[ld(0), ic(0), ld(len), ctx.call(Cv::SkipWs), st(i)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(i), ld(len), Ins::I32LtS, Ins::I32Eqz]),
        |b| {
            b.push(Ins::F64Const(0.0));
            b.push(Ins::Return);
        },
    );

    // ---- NonDecimalIntegerLiteral, which the grammar puts before any sign --
    b.extend_from_slice(&[ic(0), st(radix)]);
    if_then(
        b,
        |b| {
            str_byte(b, 0, &[ld(i)]);
            b.extend_from_slice(&[ic(48), Ins::I32Eq]);
            b.extend_from_slice(&[ld(i), ic(1), Ins::I32Add, ld(len), Ins::I32LtS, Ins::I32And]);
        },
        |b| {
            str_byte(b, 0, &[ld(i), ic(1), Ins::I32Add]);
            b.push(st(c));
            for (lo, up, r) in [(b'x', b'X', 16), (b'o', b'O', 8), (b'b', b'B', 2)] {
                if_then(
                    b,
                    |b| {
                        b.extend_from_slice(&[ld(c), ic(lo as i32), Ins::I32Eq]);
                        b.extend_from_slice(&[ld(c), ic(up as i32), Ins::I32Eq, Ins::I32Or]);
                    },
                    |b| b.extend_from_slice(&[ic(r), st(radix)]),
                );
            }
        },
    );
    if_then(
        b,
        |b| b.push(ld(radix)),
        |b| {
            b.extend_from_slice(&[ic(S2N_LIMBS), ctx.call(Cv::BnNew), st(nbig)]);
            b.extend_from_slice(&[ld(nbig), ic(0), ctx.call(Cv::BnSeti)]);
            b.extend_from_slice(&[ld(i), ic(2), Ins::I32Add, st(j)]);
            b.extend_from_slice(&[ic(0), st(cnt), ic(0), st(over), ic(0), st(stop)]);
            while_loop(
                b,
                |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                |b| {
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(j), ld(len), Ins::I32LtS, Ins::I32Eqz]),
                        |b| b.extend_from_slice(&[ic(1), st(stop)]),
                    );
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                        |b| {
                            str_byte(b, 0, &[ld(j)]);
                            b.push(st(c));
                            b.extend_from_slice(&[ic(-1), st(dv)]);
                            if_then(
                                b,
                                |b| {
                                    b.extend_from_slice(&[ld(c), ic(48), Ins::I32LtS, Ins::I32Eqz]);
                                    b.extend_from_slice(&[
                                        ic(57),
                                        ld(c),
                                        Ins::I32LtS,
                                        Ins::I32Eqz,
                                        Ins::I32And,
                                    ]);
                                },
                                |b| b.extend_from_slice(&[ld(c), ic(48), Ins::I32Sub, st(dv)]),
                            );
                            if_then(
                                b,
                                |b| {
                                    b.extend_from_slice(&[ld(radix), ic(16), Ins::I32Eq]);
                                    b.extend_from_slice(&[
                                        ld(c),
                                        ic(97),
                                        Ins::I32LtS,
                                        Ins::I32Eqz,
                                        Ins::I32And,
                                    ]);
                                    b.extend_from_slice(&[
                                        ic(102),
                                        ld(c),
                                        Ins::I32LtS,
                                        Ins::I32Eqz,
                                        Ins::I32And,
                                    ]);
                                },
                                |b| b.extend_from_slice(&[ld(c), ic(87), Ins::I32Sub, st(dv)]),
                            );
                            if_then(
                                b,
                                |b| {
                                    b.extend_from_slice(&[ld(radix), ic(16), Ins::I32Eq]);
                                    b.extend_from_slice(&[
                                        ld(c),
                                        ic(65),
                                        Ins::I32LtS,
                                        Ins::I32Eqz,
                                        Ins::I32And,
                                    ]);
                                    b.extend_from_slice(&[
                                        ic(70),
                                        ld(c),
                                        Ins::I32LtS,
                                        Ins::I32Eqz,
                                        Ins::I32And,
                                    ]);
                                },
                                |b| b.extend_from_slice(&[ld(c), ic(55), Ins::I32Sub, st(dv)]),
                            );
                            if_then(
                                b,
                                |b| {
                                    b.extend_from_slice(&[ld(dv), ic(0), Ins::I32LtS]);
                                    b.extend_from_slice(&[
                                        ld(dv),
                                        ld(radix),
                                        Ins::I32LtS,
                                        Ins::I32Eqz,
                                        Ins::I32Or,
                                    ]);
                                },
                                |b| b.extend_from_slice(&[ic(1), st(stop)]),
                            );
                            if_then(
                                b,
                                |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                                |b| {
                                    // Past about 2^1100 the answer is Infinity
                                    // whatever the remaining digits are, so the
                                    // bignum stops growing and the scan carries
                                    // on only to decide legality.
                                    if_then(
                                        b,
                                        |b| {
                                            b.push(ic(1100));
                                            b.extend_from_slice(&[ld(nbig), ctx.call(Cv::BnBits)]);
                                            b.push(Ins::I32LtS);
                                        },
                                        |b| b.extend_from_slice(&[ic(1), st(over)]),
                                    );
                                    if_then(
                                        b,
                                        |b| b.extend_from_slice(&[ld(over), Ins::I32Eqz]),
                                        |b| {
                                            b.extend_from_slice(&[
                                                ld(nbig),
                                                ld(radix),
                                                ctx.call(Cv::BnMulSmall),
                                            ]);
                                            b.extend_from_slice(&[
                                                ld(nbig),
                                                ld(dv),
                                                ctx.call(Cv::BnAddSmall),
                                            ]);
                                        },
                                    );
                                    bump(b, cnt, 1);
                                    bump(b, j, 1);
                                },
                            );
                        },
                    );
                },
            );
            if_then(b, |b| b.extend_from_slice(&[ld(cnt), Ins::I32Eqz]), nan);
            b.extend_from_slice(&[ld(0), ld(j), ld(len), ctx.call(Cv::SkipWs), st(j)]);
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(j), ld(len), Ins::I32Ne]),
                nan,
            );
            if_then(
                b,
                |b| b.push(ld(over)),
                |b| {
                    b.push(Ins::F64Const(f64::INFINITY));
                    b.push(Ins::Return);
                },
            );
            b.extend_from_slice(&[ic(S2N_LIMBS), ctx.call(Cv::BnNew), st(dbig)]);
            b.extend_from_slice(&[ic(S2N_LIMBS), ctx.call(Cv::BnNew), st(tbig)]);
            b.extend_from_slice(&[ld(dbig), ic(1), ctx.call(Cv::BnSeti)]);
            b.extend_from_slice(&[
                ld(nbig),
                ld(dbig),
                ld(tbig),
                ic(0),
                ctx.call(Cv::RatioToF64),
            ]);
            b.push(Ins::Return);
        },
    );

    // ---- sign -----------------------------------------------------------
    str_byte(b, 0, &[ld(i)]);
    b.push(st(c));
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(c), ic(43), Ins::I32Eq]),
        |b| bump(b, i, 1),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(c), ic(45), Ins::I32Eq]),
        |b| {
            b.extend_from_slice(&[ic(1), st(neg)]);
            bump(b, i, 1);
        },
    );

    // ---- Infinity -------------------------------------------------------
    if_then(
        b,
        |b| {
            // `i + 8 <= len`: the string may be exactly "Infinity" and no
            // longer, so this is a `<=` and not a `<`.
            b.extend_from_slice(&[ld(len), ld(i), ic(8), Ins::I32Add, Ins::I32LtS, Ins::I32Eqz]);
        },
        |b| {
            b.extend_from_slice(&[ic(1), st(stop)]);
            for (k, ch) in b"Infinity".iter().enumerate() {
                if_then(
                    b,
                    |b| {
                        str_byte(b, 0, &[ld(i), ic(k as i32), Ins::I32Add]);
                        b.extend_from_slice(&[ic(*ch as i32), Ins::I32Ne]);
                    },
                    |b| b.extend_from_slice(&[ic(0), st(stop)]),
                );
            }
            if_then(
                b,
                |b| b.push(ld(stop)),
                |b| {
                    b.extend_from_slice(&[
                        ld(0),
                        ld(i),
                        ic(8),
                        Ins::I32Add,
                        ld(len),
                        ctx.call(Cv::SkipWs),
                        st(j),
                    ]);
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(j), ld(len), Ins::I32Ne]),
                        nan,
                    );
                    signed(b, f64::INFINITY);
                },
            );
        },
    );

    // ---- StrDecimalLiteral ----------------------------------------------
    // One byte per kept digit, and there can be no more digits than there are
    // bytes in the string, so `"7"` pays one byte rather than 768.
    b.extend_from_slice(&[ld(len), st(j)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ic(MAX_DIGITS), ld(j), Ins::I32LtS]),
        |b| b.extend_from_slice(&[ic(MAX_DIGITS), st(j)]),
    );
    b.extend_from_slice(&[ld(j), ctx.alloc(), st(dbuf)]);
    b.extend_from_slice(&[ic(0), st(gg), ic(-1), st(pp), ic(0), st(lz)]);
    b.extend_from_slice(&[
        ic(0),
        st(nd),
        ic(0),
        st(seen),
        ic(0),
        st(sticky),
        ic(0),
        st(stop),
    ]);
    while_loop(
        b,
        |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
        |b| {
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(i), ld(len), Ins::I32LtS, Ins::I32Eqz]),
                |b| b.extend_from_slice(&[ic(1), st(stop)]),
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                |b| {
                    str_byte(b, 0, &[ld(i)]);
                    b.push(st(c));
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(c), ic(46), Ins::I32Eq]),
                        |b| {
                            if_then(
                                b,
                                |b| b.extend_from_slice(&[ld(pp), ic(0), Ins::I32LtS, Ins::I32Eqz]),
                                |b| b.extend_from_slice(&[ic(1), st(stop)]),
                            );
                            if_then(
                                b,
                                |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                                |b| {
                                    b.extend_from_slice(&[ld(gg), st(pp)]);
                                    bump(b, i, 1);
                                    b.extend_from_slice(&[ic(2), st(stop)]);
                                },
                            );
                        },
                    );
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                        |b| {
                            if_then(
                                b,
                                |b| {
                                    b.extend_from_slice(&[ld(c), ic(48), Ins::I32LtS]);
                                    b.extend_from_slice(&[ic(57), ld(c), Ins::I32LtS, Ins::I32Or]);
                                },
                                |b| b.extend_from_slice(&[ic(1), st(stop)]),
                            );
                            if_then(
                                b,
                                |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                                |b| {
                                    if_then(
                                        b,
                                        |b| {
                                            b.extend_from_slice(&[ld(c), ic(48), Ins::I32Eq]);
                                            b.extend_from_slice(&[
                                                ld(seen),
                                                Ins::I32Eqz,
                                                Ins::I32And,
                                            ]);
                                        },
                                        |b| bump(b, lz, 1),
                                    );
                                    if_then(
                                        b,
                                        |b| {
                                            b.extend_from_slice(&[ld(c), ic(48), Ins::I32Eq]);
                                            b.extend_from_slice(&[
                                                ld(seen),
                                                Ins::I32Eqz,
                                                Ins::I32And,
                                                Ins::I32Eqz,
                                            ]);
                                        },
                                        |b| {
                                            b.extend_from_slice(&[ic(1), st(seen)]);
                                            if_then(
                                                b,
                                                |b| {
                                                    b.extend_from_slice(&[
                                                        ld(nd),
                                                        ic(MAX_DIGITS),
                                                        Ins::I32LtS,
                                                    ])
                                                },
                                                |b| {
                                                    b.extend_from_slice(&[
                                                        ld(dbuf),
                                                        ld(nd),
                                                        Ins::I32Add,
                                                    ]);
                                                    b.extend_from_slice(&[
                                                        ld(c),
                                                        ic(48),
                                                        Ins::I32Sub,
                                                    ]);
                                                    b.push(Ins::I32Store8(0, 0));
                                                    bump(b, nd, 1);
                                                },
                                            );
                                            if_then(
                                                b,
                                                |b| {
                                                    b.extend_from_slice(&[
                                                        ld(nd),
                                                        ic(MAX_DIGITS),
                                                        Ins::I32LtS,
                                                        Ins::I32Eqz,
                                                    ]);
                                                    b.extend_from_slice(&[
                                                        ld(c),
                                                        ic(48),
                                                        Ins::I32Ne,
                                                        Ins::I32And,
                                                    ]);
                                                },
                                                |b| b.extend_from_slice(&[ic(1), st(sticky)]),
                                            );
                                        },
                                    );
                                    bump(b, gg, 1);
                                    bump(b, i, 1);
                                },
                            );
                        },
                    );
                    // A point consumed this turn: reset the sentinel.
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(stop), ic(2), Ins::I32Eq]),
                        |b| b.extend_from_slice(&[ic(0), st(stop)]),
                    );
                },
            );
        },
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(pp), ic(0), Ins::I32LtS]),
        |b| b.extend_from_slice(&[ld(gg), st(pp)]),
    );
    if_then(b, |b| b.extend_from_slice(&[ld(gg), Ins::I32Eqz]), nan);
    b.extend_from_slice(&[ld(pp), ld(lz), Ins::I32Sub, ld(nd), Ins::I32Sub, st(e10)]);

    // ---- ExponentPart ---------------------------------------------------
    b.extend_from_slice(&[ic(0), st(stop)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(i), ld(len), Ins::I32LtS]),
        |b| {
            str_byte(b, 0, &[ld(i)]);
            b.push(st(c));
            b.extend_from_slice(&[ld(c), ic(101), Ins::I32Eq]);
            b.extend_from_slice(&[ld(c), ic(69), Ins::I32Eq, Ins::I32Or, st(stop)]);
        },
    );
    if_then(
        b,
        |b| b.push(ld(stop)),
        |b| {
            b.extend_from_slice(&[ld(i), ic(1), Ins::I32Add, st(j)]);
            b.extend_from_slice(&[ic(1), st(esign)]);
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(j), ld(len), Ins::I32LtS]),
                |b| {
                    str_byte(b, 0, &[ld(j)]);
                    b.push(st(c));
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(c), ic(43), Ins::I32Eq]),
                        |b| bump(b, j, 1),
                    );
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(c), ic(45), Ins::I32Eq]),
                        |b| {
                            b.extend_from_slice(&[ic(-1), st(esign)]);
                            bump(b, j, 1);
                        },
                    );
                },
            );
            // An `e` with no digits after it is not an ExponentPart, and the
            // grammar has nothing else that could consume the `e`, so the
            // whole string fails rather than the `e` being ignored.
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(j), ld(len), Ins::I32LtS, Ins::I32Eqz]),
                nan,
            );
            str_byte(b, 0, &[ld(j)]);
            b.push(st(c));
            if_then(
                b,
                |b| {
                    b.extend_from_slice(&[ld(c), ic(48), Ins::I32LtS]);
                    b.extend_from_slice(&[ic(57), ld(c), Ins::I32LtS, Ins::I32Or]);
                },
                nan,
            );
            b.extend_from_slice(&[ic(0), st(ev), ic(0), st(stop)]);
            while_loop(
                b,
                |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                |b| {
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(j), ld(len), Ins::I32LtS, Ins::I32Eqz]),
                        |b| b.extend_from_slice(&[ic(1), st(stop)]),
                    );
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                        |b| {
                            str_byte(b, 0, &[ld(j)]);
                            b.push(st(c));
                            if_then(
                                b,
                                |b| {
                                    b.extend_from_slice(&[ld(c), ic(48), Ins::I32LtS]);
                                    b.extend_from_slice(&[ic(57), ld(c), Ins::I32LtS, Ins::I32Or]);
                                },
                                |b| b.extend_from_slice(&[ic(1), st(stop)]),
                            );
                            if_then(
                                b,
                                |b| b.extend_from_slice(&[ld(stop), Ins::I32Eqz]),
                                |b| {
                                    if_then(
                                        b,
                                        |b| {
                                            b.extend_from_slice(&[ld(ev), ic(1000000), Ins::I32LtS])
                                        },
                                        |b| {
                                            b.extend_from_slice(&[ld(ev), ic(10), Ins::I32Mul]);
                                            b.extend_from_slice(&[ld(c), ic(48), Ins::I32Sub]);
                                            b.extend_from_slice(&[Ins::I32Add, st(ev)]);
                                        },
                                    );
                                    bump(b, j, 1);
                                },
                            );
                        },
                    );
                },
            );
            b.extend_from_slice(&[ld(j), st(i)]);
            b.extend_from_slice(&[
                ld(e10),
                ld(esign),
                ld(ev),
                Ins::I32Mul,
                Ins::I32Add,
                st(e10),
            ]);
        },
    );

    // ---- trailing whitespace, then the number itself ---------------------
    b.extend_from_slice(&[ld(0), ld(i), ld(len), ctx.call(Cv::SkipWs), st(i)]);
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(i), ld(len), Ins::I32Ne]),
        nan,
    );
    // Every digit was a zero: the value is a signed zero, and `-0` is a real
    // answer here rather than a rounding of one.
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(nd), Ins::I32Eqz]),
        |b| signed(b, 0.0),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ic(2000000), ld(e10), Ins::I32LtS]),
        |b| b.extend_from_slice(&[ic(2000000), st(e10)]),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(e10), ic(-2000000), Ins::I32LtS]),
        |b| b.extend_from_slice(&[ic(-2000000), st(e10)]),
    );
    // The value is in [10^(dx-1), 10^dx), so these two bounds are decided
    // before any bignum is built -- 10^309 is past the largest binary64 and
    // 10^-324 is below half the smallest subnormal.
    if_then(
        b,
        |b| b.extend_from_slice(&[ic(309), ld(nd), ld(e10), Ins::I32Add, Ins::I32LtS]),
        |b| signed(b, f64::INFINITY),
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(nd), ld(e10), Ins::I32Add, ic(-323), Ins::I32LtS]),
        |b| signed(b, 0.0),
    );
    b.extend_from_slice(&[
        ld(dbuf),
        ld(nd),
        ld(e10),
        ld(sticky),
        ctx.call(Cv::DigitsToF64),
        st(val),
    ]);
    if_then(
        b,
        |b| b.push(ld(neg)),
        |b| {
            b.extend_from_slice(&[ld(val), Ins::F64Neg]);
            b.push(Ins::Return);
        },
    );
    b.push(ld(val));
    f
}

// -------------------------------------------------------------------------
// String relational comparison
// -------------------------------------------------------------------------

/// `__u16_next(s, i, pend) -> (unit, i2, pend2)`.
///
/// One UTF-16 code unit at a time out of UTF-8 bytes, with `pend` carrying the
/// low surrogate of a supplementary character across a call. `-1` is the end;
/// a real unit is `0 ..= 0xFFFF`, so the sentinel cannot collide with one.
fn u16_next() -> FnBuild {
    let mut f = FnBuild::new(3);
    let len = f.local(ValType::I32);
    let b0 = f.local(ValType::I32);
    let cp = f.local(ValType::I32);
    let b = &mut f.body;
    let cont = |b: &mut Vec<Ins>, k: i32| {
        str_byte(b, 0, &[ld(1), ic(k), Ins::I32Add]);
        b.extend_from_slice(&[ic(0x3F), Ins::I32And]);
    };
    if_then(
        b,
        |b| b.push(ld(2)),
        |b| {
            b.extend_from_slice(&[ld(2), ld(1), ic(0)]);
            b.push(Ins::Return);
        },
    );
    b.push(ld(0));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(st(len));
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(1), ld(len), Ins::I32LtS, Ins::I32Eqz]),
        |b| {
            b.extend_from_slice(&[ic(-1), ld(1), ic(0)]);
            b.push(Ins::Return);
        },
    );
    str_byte(b, 0, &[ld(1)]);
    b.push(st(b0));
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(b0), ic(0x80), Ins::I32LtS]),
        |b| {
            b.extend_from_slice(&[ld(b0), ld(1), ic(1), Ins::I32Add, ic(0)]);
            b.push(Ins::Return);
        },
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(b0), ic(0xE0), Ins::I32LtS]),
        |b| {
            b.extend_from_slice(&[ld(b0), ic(0x1F), Ins::I32And, ic(6), Ins::I32Shl]);
            cont(b, 1);
            b.push(Ins::I32Or);
            b.extend_from_slice(&[ld(1), ic(2), Ins::I32Add, ic(0)]);
            b.push(Ins::Return);
        },
    );
    if_then(
        b,
        |b| b.extend_from_slice(&[ld(b0), ic(0xF0), Ins::I32LtS]),
        |b| {
            b.extend_from_slice(&[ld(b0), ic(0x0F), Ins::I32And, ic(12), Ins::I32Shl]);
            cont(b, 1);
            b.extend_from_slice(&[ic(6), Ins::I32Shl, Ins::I32Or]);
            cont(b, 2);
            b.push(Ins::I32Or);
            b.extend_from_slice(&[ld(1), ic(3), Ins::I32Add, ic(0)]);
            b.push(Ins::Return);
        },
    );
    // Supplementary: two surrogates, and the high one is what orders the
    // character. That is the whole reason this is not a byte compare.
    b.extend_from_slice(&[ld(b0), ic(0x07), Ins::I32And, ic(18), Ins::I32Shl]);
    cont(b, 1);
    b.extend_from_slice(&[ic(12), Ins::I32Shl, Ins::I32Or]);
    cont(b, 2);
    b.extend_from_slice(&[ic(6), Ins::I32Shl, Ins::I32Or]);
    cont(b, 3);
    b.extend_from_slice(&[Ins::I32Or, ic(0x10000), Ins::I32Sub, st(cp)]);
    b.extend_from_slice(&[ld(cp), ic(1024), Ins::I32DivS, ic(0xD800), Ins::I32Add]);
    b.extend_from_slice(&[ld(1), ic(4), Ins::I32Add]);
    b.extend_from_slice(&[ld(cp), ic(0x3FF), Ins::I32And, ic(0xDC00), Ins::I32Add]);
    f
}

/// `__str_cmp(a, b) -> -1 | 0 | 1`: ECMA-262 7.2.13's code-unit order, which
/// is neither byte order nor locale order.
fn str_cmp(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2);
    let ia = f.local(ValType::I32);
    let ib = f.local(ValType::I32);
    let pa = f.local(ValType::I32);
    let pb = f.local(ValType::I32);
    let ua = f.local(ValType::I32);
    let ub = f.local(ValType::I32);
    let b = &mut f.body;
    b.extend_from_slice(&[ic(0), st(ia), ic(0), st(ib), ic(0), st(pa), ic(0), st(pb)]);
    while_loop(
        b,
        |b| b.push(ic(1)),
        |b| {
            b.extend_from_slice(&[ld(0), ld(ia), ld(pa), ctx.call(Cv::U16Next)]);
            b.extend_from_slice(&[st(pa), st(ia), st(ua)]);
            b.extend_from_slice(&[ld(1), ld(ib), ld(pb), ctx.call(Cv::U16Next)]);
            b.extend_from_slice(&[st(pb), st(ib), st(ub)]);
            if_then(
                b,
                |b| {
                    b.extend_from_slice(&[ld(ua), ic(0), Ins::I32LtS]);
                    b.extend_from_slice(&[ld(ub), ic(0), Ins::I32LtS, Ins::I32And]);
                },
                |b| {
                    b.push(ic(0));
                    b.push(Ins::Return);
                },
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(ua), ic(0), Ins::I32LtS]),
                |b| {
                    b.push(ic(-1));
                    b.push(Ins::Return);
                },
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(ub), ic(0), Ins::I32LtS]),
                |b| {
                    b.push(ic(1));
                    b.push(Ins::Return);
                },
            );
            if_then(
                b,
                |b| b.extend_from_slice(&[ld(ua), ld(ub), Ins::I32Ne]),
                |b| {
                    if_then(
                        b,
                        |b| b.extend_from_slice(&[ld(ua), ld(ub), Ins::I32LtS]),
                        |b| {
                            b.push(ic(-1));
                            b.push(Ins::Return);
                        },
                    );
                    b.push(ic(1));
                    b.push(Ins::Return);
                },
            );
        },
    );
    b.push(Ins::Unreachable);
    f
}

// =========================================================================
// JSON (ECMA-262 25.5)
// =========================================================================
//
// # `JSON` is an object, not an instruction
//
// Everything below is reachable from a script only through an ordinary
// Object holding two ordinary function values: [`Js::Ns`] builds it,
// `__obj_get` reads a property off it, and the call goes through
// `call_indirect` on the same adapter table every other function value uses.
// Nothing in `emit` needs to know the name `JSON.stringify`; it needs to know
// that the binding `JSON` is initialised by one call. That was not a stylistic
// preference — it is what the milestone that landed objects, function values
// and indirect calls bought, and spending it on an intrinsic would have been
// paying twice.
//
// The urge to make it an intrinsic was concrete and is recorded rather than
// satisfied: `JSON.stringify(o)` is a *statically known* callee at every one
// of its nine sites in the acceptance target, so a direct call would save the
// property read, the tag test and the `call_indirect` at each. It is refused
// on the grounds `super::repr`'s header gives for the dispatch order: an
// exemption written into one call site is an exemption the compiler has no
// pass to check, and the moment a script writes `const f = JSON.stringify` the
// two spellings would have to agree. The cure, when the cost is measured and
// matters, is a general devirtualisation pass over a property read of a
// known-constant object — one place, testable — not a name the lowering
// recognises.
//
// # This set is gated
//
// [`SET`] is unconditional because a program that adds two values may need
// `Number::toString` and the compiler cannot tell. `JSON` is the opposite: the
// predicate is "does the program mention the name `JSON`", which a scan
// settles exactly, with no approximation and no false negative. So
// [`JSON_SET`] is a *second* set the lowering places only when the answer is
// yes, and a program that never writes `JSON` is byte-for-byte what it was.
//
// # Where the spec is followed against the obvious
//
// - **U+2028 and U+2029 are not escaped.** 25.5.2.2 QuoteJSONString escapes
//   the seven characters of its table, everything below U+0020, and lone
//   surrogates — and nothing else. The line separators are ordinary
//   characters *in JSON*; escaping them is a habit from embedding JSON in
//   JavaScript source, which is a different problem with a different owner.
//   Adding them here would make this engine's output disagree with every
//   other `JSON.stringify` for no reader's benefit.
// - **The lone-surrogate arm of QuoteJSONString is absent, and that is a
//   citation and not an omission.** A guest String is UTF-8, and the only
//   producers are the lexer — which refuses unpaired surrogates outright
//   (`lex.rs`, `UNPAIRED_SURROGATE`) — and a host's `Bytes` result, which is
//   UTF-8 by the door's contract. There is no reachable input for the arm.
//   `JSON.parse` therefore *refuses* `"\ud800"` rather than fabricating a
//   string it cannot hold, and says so as a capability boundary.
// - **Arrays are refused, not approximated.** This engine has no Array, so
//   `[1,2]` is not "invalid JSON" and must not be reported as if it were. It
//   is the one refusal in the parser worded as the engine's boundary, and it
//   is the single largest thing standing between this and the acceptance
//   target's real traffic.
//
// # Throwing
//
// ECMA-262 makes a bad `JSON.parse` a **SyntaxError** and a cycle a
// **TypeError**, and both have to be *catchable*, because the library this
// milestone exists to compile wraps `JSON.parse` in a `try` and falls back to
// the raw text. Every one of those exits funnels through exactly two
// functions — [`Js::Fail`], which names the message, and [`Js::Throw`], which
// is the engine's single abrupt-completion point.
//
// `__throw` has two bodies and [`JsonCtx::unwind`] chooses:
//
// * **With an unwind channel** — the three module globals `emit` allocates for
//   `throw`/`try` — it parks the value in them, raises the flag and
//   *returns*. Every caller in this set then checks the flag and returns in
//   turn, which is the same propagation `emit` compiles for a user function,
//   so a `catch` in the script sees a failed `JSON.parse` exactly as it sees a
//   `throw`.
// * **Without one** it records [`super::runtime::FAULT_UNCAUGHT_THROW`] and
//   traps. That is not a degraded mode to be embarrassed about: a module with
//   no channel has no `catch` either, so the throw was going to be uncaught,
//   and this is what an uncaught throw does at the top of the script too.
//
// The propagation checks are the only thing the flag costs this set, and they
// are emitted **only** in the first mode. There are seven of them, at the
// seven calls after which work continues; a call whose result is returned
// immediately needs none, because the caller's own check discards whatever
// came back.
//
// `__throw` is deliberately not an [`Rt`]: promoting it to the unconditional
// runtime set is where it belongs, and doing it now would change the byte
// count of every module ever compiled along with the size assertions that pin
// them.

/// Everything above, as one module, so that the whole set can be marked
/// unused in one place: `emit` does not call [`build_json`] yet -- wiring it
/// is the hook this milestone hands the integrator -- and until it does,
/// every function here is dead code that is not a defect. The attribute comes
/// off with the hook.
#[allow(dead_code)]
pub(crate) mod json {
    use super::*;
    // Relative rather than `crate::`, for the reason this file's own
    // `use super::runtime::{...}` is: `tests/json.rs` and friends pull these
    // modules in with `#[path]`, so inside a test binary `convert` is a
    // top-level module and `crate::array` is not a path that exists.
    use super::super::array::{ARR_ELEMS, ARR_LEN, Ar, ELEM_BYTES, ELEM_PAYLOAD, ELEM_TAG};
    use super::super::repr::{box_array, is_array, unbox_array};

    /// The string buffer these algorithms build their answers in:
    /// `[len: i32][cap: i32][data: i32]`, over a raw byte array.
    ///
    /// # Why a buffer and not `__str_concat`
    ///
    /// Concatenation is what the runtime already has, and it is the wrong tool
    /// twice over here. Quoting a string appends one to six bytes per source
    /// byte, so a concatenation per byte would allocate the whole result once per
    /// character — quadratic in allocation *and* in copying, on a bump heap that
    /// never gives anything back. Serializing an object appends a piece per
    /// property and per punctuation mark. One growable buffer makes both linear,
    /// and it is the same five functions for both, which is why it is here rather
    /// than a special case in either.
    const JB_LEN: u32 = 0;
    const JB_CAP: u32 = 4;
    const JB_DATA: u32 = 8;
    const JB_HEADER: i32 = 12;
    /// The first data allocation. Sized for the acceptance target's parameter
    /// objects — `{"tab":"t3","note":"…"}` and its six siblings — so the common
    /// case never grows at all.
    const JB_FIRST: i32 = 64;

    /// The parser's whole state: `[src: i32][pos: i32]`.
    ///
    /// A record and not two more parameters on every function, because the
    /// position has to be *shared*: a nested `__json_pval` moves the same cursor
    /// its caller will read next. Two by-value parameters would be two cursors.
    const PS_SRC: u32 = 0;
    const PS_POS: u32 = 4;
    const PS_BYTES: i32 = 8;

    /// One frame of the cycle check: `[parent: i32][obj: i32]`.
    ///
    /// The chain is the *ancestors* of the value being serialized, not the set of
    /// objects already seen. A DAG — the same object reached twice by two
    /// different paths — is perfectly good JSON and must serialize twice;
    /// only an object that contains itself is 25.5.2.2's TypeError.
    const CY_PARENT: u32 = 0;
    const CY_OBJ: u32 = 4;
    const CY_BYTES: i32 = 8;

    /// The emitted JSON functions, in index order. Position in [`JSON_SET`] is the
    /// offset from [`JsonCtx::func_base`], exactly as [`Cv`] is for [`SET`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Js {
        /// The engine's single abrupt-completion point. Not JSON's.
        Throw,
        Fail,
        // the string buffer
        JbNew,
        JbRoom,
        JbByte,
        JbStr,
        JbTake,
        // 25.5.2, SerializeJSONProperty and friends
        Quote,
        Ser,
        SerObj,
        SerArr,
        Stringify,
        // 25.5.1, the JSON grammar
        PAt,
        PWs,
        PLit,
        PHex4,
        PUtf8,
        PStr,
        PNum,
        PObj,
        PArr,
        PVal,
        Parse,
        /// The namespace object itself.
        Ns,
    }

    /// Every JSON function, in the order they are defined in the module.
    pub(crate) const JSON_SET: &[Js] = &[
        Js::Throw,
        Js::Fail,
        Js::JbNew,
        Js::JbRoom,
        Js::JbByte,
        Js::JbStr,
        Js::JbTake,
        Js::Quote,
        Js::Ser,
        Js::SerObj,
        Js::SerArr,
        Js::Stringify,
        Js::PAt,
        Js::PWs,
        Js::PLit,
        Js::PHex4,
        Js::PUtf8,
        Js::PStr,
        Js::PNum,
        Js::PObj,
        Js::PArr,
        Js::PVal,
        Js::Parse,
        Js::Ns,
    ];

    impl Js {
        /// The name the function is given in the module. Not exported, for the
        /// reason [`Rt::symbol`] gives.
        pub(crate) fn symbol(self) -> &'static str {
            match self {
                Js::Throw => "__throw",
                Js::Fail => "__json_fail",
                Js::JbNew => "__jb_new",
                Js::JbRoom => "__jb_room",
                Js::JbByte => "__jb_byte",
                Js::JbStr => "__jb_str",
                Js::JbTake => "__jb_take",
                Js::Quote => "__json_quote",
                Js::Ser => "__json_ser",
                Js::SerObj => "__json_ser_obj",
                Js::SerArr => "__json_ser_arr",
                Js::Stringify => "__json_stringify",
                Js::PAt => "__jp_at",
                Js::PWs => "__jp_ws",
                Js::PLit => "__jp_lit",
                Js::PHex4 => "__jp_hex4",
                Js::PUtf8 => "__jp_utf8",
                Js::PStr => "__json_pstr",
                Js::PNum => "__json_pnum",
                Js::PObj => "__json_pobj",
                Js::PArr => "__json_parr",
                Js::PVal => "__json_pval",
                Js::Parse => "__json_parse",
                Js::Ns => "__json_ns",
            }
        }

        /// Offset of this function from [`JsonCtx::func_base`].
        pub(crate) fn offset(self) -> u32 {
            JSON_SET
                .iter()
                .position(|j| *j == self)
                .expect("JSON_SET lists every Js") as u32
        }
    }

    /// Every fixed String this set names, as guest addresses in the pool.
    ///
    /// The two property keys are here rather than in the lowering because the
    /// object is built here: `__json_ns` stores them, and a second interning of
    /// the same text in `emit` would be the same record anyway
    /// ([`StringPool::intern`] shares) but a second place to keep it right.
    ///
    /// # The messages name the engine's boundary where the engine is the limit
    ///
    /// `array` and `surrogate` are refusals of things this engine cannot
    /// represent, and they say so. The rest are refusals of text that is not
    /// JSON, and they say *that* — a sentence blaming the engine for a genuine
    /// syntax error would send the reader hunting for a workaround that does not
    /// exist, which is the mirror image of the defect `conformance_m2.rs`
    /// records for the parser's own diagnostics.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct JsonNames {
        pub(crate) stringify: i32,
        pub(crate) parse: i32,
        /// Also the answer for a non-finite Number — 25.5.2.2 step 10.
        pub(crate) null: i32,
        pub(crate) yes: i32,
        pub(crate) no: i32,
        pub(crate) syntax: i32,
        pub(crate) eof: i32,
        pub(crate) surrogate: i32,
        pub(crate) cycle: i32,
        pub(crate) replacer: i32,
        pub(crate) reviver: i32,
    }

    impl JsonNames {
        pub(crate) fn intern(pool: &mut StringPool) -> Self {
            Self {
                stringify: pool.intern("stringify"),
                parse: pool.intern("parse"),
                null: pool.intern("null"),
                yes: pool.intern("true"),
                no: pool.intern("false"),
                syntax: pool.intern("JSON.parse: unexpected token"),
                eof: pool.intern("JSON.parse: unexpected end of input"),
                surrogate: pool
                    .intern("this engine does not support unpaired surrogates in JSON text yet"),
                cycle: pool.intern("JSON.stringify: converting circular structure to JSON"),
                replacer: pool.intern(
                    "this engine does not support a JSON.stringify replacer or space argument yet",
                ),
                reviver: pool.intern("this engine does not support a JSON.parse reviver yet"),
            }
        }
    }

    /// The three module globals a throw in flight lives in, as `super::emit`
    /// allocates them.
    ///
    /// A copy of the indices and not of `emit`'s own type, because this module
    /// must not depend on the lowering — the same reason [`super::runtime`]'s
    /// [`Conversions`](super::runtime::Conversions) holds indices rather than a
    /// [`Cv`]. Three `u32`s is the whole contract.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Throwing {
        /// `1` while a throw is looking for its handler.
        pub(crate) flag: u32,
        pub(crate) tag: u32,
        pub(crate) payload: u32,
    }

    /// What this set needs to know about the module it is spliced into.
    pub(crate) struct JsonCtx {
        /// Function index of `__throw` — the first of [`JSON_SET`].
        pub(crate) func_base: u32,
        /// Function index of `__add` — the first of [`super::runtime::SET`].
        pub(crate) runtime_base: u32,
        /// Function index of `__bn_new` — the first of [`SET`].
        pub(crate) convert_base: u32,
        /// Where a throw in flight lives, or `None` for a module with no unwind
        /// channel. See this section's *Throwing*.
        ///
        /// **A program that mentions `JSON` needs the channel whether or not it
        /// writes `throw`**, because `JSON.parse` can raise one. That is a
        /// condition on `emit`'s scan and not something this module can enforce;
        /// with `None` the refusals are still refusals, they are just not
        /// catchable.
        pub(crate) unwind: Option<Throwing>,
        /// Function index of `__arr_new` — the first of
        /// [`super::super::array::SET`].
        ///
        /// Not an `Option`: `emit`'s scan sets its array gate from
        /// `arrays |= json`, so a module that has this set has that one too.
        /// `JSON.parse` is exactly why -- it can return an array from text no
        /// `[` appears in, which is the half of the array predicate that is
        /// not about the source's syntax.
        pub(crate) arrays: u32,
        pub(crate) names: JsonNames,
    }

    impl JsonCtx {
        pub(crate) fn call(&self, js: Js) -> Ins {
            Ins::Call(self.func_base + js.offset())
        }

        fn rt(&self, rt: Rt) -> Ins {
            Ins::Call(self.runtime_base + rt.offset())
        }

        fn cv(&self, cv: Cv) -> Ins {
            Ins::Call(self.convert_base + cv.offset())
        }

        fn ar(&self, ar: Ar) -> Ins {
            Ins::Call(self.arrays + ar.offset())
        }

        fn alloc(&self) -> Ins {
            self.rt(Rt::Alloc)
        }

        /// Build this set's `JsonCtx` from the other two, given where all three
        /// were placed. Kept for the same reason [`Ctx::beside`] is.
        #[allow(dead_code)]
        pub(crate) fn beside(
            rt: &RtCtx,
            cv: &Ctx,
            func_base: u32,
            unwind: Option<Throwing>,
            names: JsonNames,
        ) -> Self {
            Self {
                func_base,
                runtime_base: rt.func_base,
                convert_base: cv.func_base,
                unwind,
                arrays: func_base + JSON_SET.len() as u32,
                names,
            }
        }
    }

    /// Build every JSON function, in [`JSON_SET`] order. Called only for a
    /// program that mentions `JSON` — see this section's header.
    pub(crate) fn build_json(ctx: &JsonCtx) -> Vec<RtFunc> {
        JSON_SET.iter().map(|js| one_json(ctx, *js)).collect()
    }

    fn one_json(ctx: &JsonCtx, js: Js) -> RtFunc {
        use ValType::{F64, I32, I64};
        let (params, results, f) = match js {
            Js::Throw => (jvalues(1), vec![], throw(ctx)),
            Js::Fail => (vec![I32], vec![], json_fail_fn(ctx)),
            Js::JbNew => (vec![], vec![I32], jb_new(ctx)),
            Js::JbRoom => (vec![I32, I32], vec![], jb_room(ctx)),
            Js::JbByte => (vec![I32, I32], vec![], jb_byte(ctx)),
            Js::JbStr => (vec![I32, I32], vec![], jb_str(ctx)),
            Js::JbTake => (vec![I32], vec![I32], jb_take(ctx)),
            Js::Quote => (vec![I32, I32], vec![], json_quote(ctx)),
            Js::Ser => (vec![I32, I64, I32, I32], vec![I32], json_ser(ctx)),
            Js::SerObj => (vec![I32, I32, I32], vec![], json_ser_obj(ctx)),
            Js::SerArr => (vec![I32, I32, I32], vec![], json_ser_arr(ctx)),
            Js::Stringify => (jvalues(3), jvalues(1), json_stringify(ctx)),
            Js::PAt => (vec![I32], vec![I32], jp_at()),
            Js::PWs => (vec![I32], vec![], jp_ws(ctx)),
            Js::PLit => (vec![I32, I32], vec![I32], jp_lit()),
            Js::PHex4 => (vec![I32], vec![I32], jp_hex4(ctx)),
            Js::PUtf8 => (vec![I32, I32], vec![], jp_utf8(ctx)),
            Js::PStr => (vec![I32], vec![I32], json_pstr(ctx)),
            Js::PNum => (vec![I32], vec![F64], json_pnum(ctx)),
            Js::PObj => (vec![I32], jvalues(1), json_pobj(ctx)),
            Js::PArr => (vec![I32], jvalues(1), json_parr(ctx)),
            Js::PVal => (vec![I32], jvalues(1), json_pval(ctx)),
            Js::Parse => (jvalues(2), jvalues(1), json_parse(ctx)),
            Js::Ns => (vec![I32, I32], vec![I32], json_ns(ctx)),
        };
        RtFunc {
            name: js.symbol(),
            params,
            results,
            locals: f.local_groups(),
            body: f.body,
        }
    }

    /// `n` JS values, flattened into wasm value types.
    fn jvalues(n: usize) -> Vec<ValType> {
        (0..n).flat_map(|_| repr::SLOTS).collect()
    }

    // ---- abbreviations for the algorithms below -----------------------------

    /// Push the byte at the cursor, or `-1` at the end of the text.
    fn at_byte(ctx: &JsonCtx, b: &mut Vec<Ins>, state: u32) {
        b.push(ld(state));
        b.push(ctx.call(Js::PAt));
    }

    /// `state.pos += n`.
    fn advance(b: &mut Vec<Ins>, state: u32, n: i32) {
        b.push(ld(state));
        b.push(ld(state));
        b.push(Ins::I32Load(ALIGN_WORD, PS_POS));
        b.push(ic(n));
        b.push(Ins::I32Add);
        b.push(Ins::I32Store(ALIGN_WORD, PS_POS));
    }

    fn skip_ws_at(ctx: &JsonCtx, b: &mut Vec<Ins>, state: u32) {
        b.push(ld(state));
        b.push(ctx.call(Js::PWs));
    }

    /// Append one literal byte to the buffer held in local `buf`.
    fn put(ctx: &JsonCtx, b: &mut Vec<Ins>, buf: u32, byte: i32) {
        b.push(ld(buf));
        b.push(ic(byte));
        b.push(ctx.call(Js::JbByte));
    }

    /// Append the bytes of the interned string at `text`.
    fn puts(ctx: &JsonCtx, b: &mut Vec<Ins>, buf: u32, text: i32) {
        b.push(ld(buf));
        b.push(ic(text));
        b.push(ctx.call(Js::JbStr));
    }

    /// What the enclosing function has to leave on the stack in order to return.
    ///
    /// A throw has to be able to leave every one of these functions, and wasm
    /// wants the result types even on a path whose value nobody will look at —
    /// the caller's flag check discards it. So the *shape* of the answer is
    /// carried down to each exit rather than each exit knowing its function.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Ret {
        None,
        I32,
        F64,
        Value,
    }

    fn ret_dummy(b: &mut Vec<Ins>, ret: Ret) {
        match ret {
            Ret::None => {}
            Ret::I32 => b.push(ic(0)),
            Ret::F64 => b.push(Ins::F64Const(0.0)),
            Ret::Value => const_undefined(b),
        }
    }

    /// Throw the interned message and leave. Every exit from both algorithms is
    /// one of these.
    ///
    /// With an unwind channel the throw is in flight and the function returns, so
    /// the caller's check can see it; without one `__throw` has already trapped
    /// and the `unreachable` is what tells the validator so.
    fn fail(ctx: &JsonCtx, b: &mut Vec<Ins>, message: i32, ret: Ret) {
        b.push(ic(message));
        b.push(ctx.call(Js::Fail));
        match ctx.unwind {
            Some(_) => {
                ret_dummy(b, ret);
                b.push(Ins::Return);
            }
            None => b.push(Ins::Unreachable),
        }
    }

    /// The propagation check after a call that could have thrown, in the shape
    /// `super::emit` compiles at a user call site: read the flag, leave if it is
    /// up. Nothing at all in a module with no unwind channel, where `__throw`
    /// never returns.
    ///
    /// It is emitted **only** where work continues afterwards. A call whose result
    /// is handed straight back needs none: the value is garbage exactly when the
    /// flag is up, and the caller's own check is what throws it away.
    fn check(ctx: &JsonCtx, b: &mut Vec<Ins>, ret: Ret) {
        let Some(unwind) = ctx.unwind else {
            return;
        };
        b.push(Ins::GlobalGet(unwind.flag));
        b.push(Ins::If(BlockType::Empty));
        ret_dummy(b, ret);
        b.push(Ins::Return);
        b.push(Ins::End);
    }

    /// `47 < c && c < 58` — one JSON digit, over a byte already in a local.
    fn digit(b: &mut Vec<Ins>, c: u32) {
        b.push(ic(47));
        b.push(ld(c));
        b.push(Ins::I32LtS);
        b.push(ld(c));
        b.push(ic(58));
        b.push(Ins::I32LtS);
        b.push(Ins::I32And);
    }

    /// Read the byte at the cursor into `c` and leave "it is a digit".
    fn digit_at(ctx: &JsonCtx, b: &mut Vec<Ins>, state: u32, c: u32) {
        at_byte(ctx, b, state);
        b.push(st(c));
        digit(b, c);
    }

    /// `lo < v && v < hi`, over a value already in a local.
    fn between(b: &mut Vec<Ins>, v: u32, lo: i32, hi: i32) {
        b.push(ic(lo));
        b.push(ld(v));
        b.push(Ins::I32LtS);
        b.push(ld(v));
        b.push(ic(hi));
        b.push(Ins::I32LtS);
        b.push(Ins::I32And);
    }

    // ---- the throw point -----------------------------------------------------

    /// `__throw(value)`: the engine's one abrupt completion.
    ///
    /// One function with two bodies rather than two call graphs, which is the
    /// whole point of routing every refusal through it: the difference between a
    /// catchable throw and a trap is *here*, and nowhere among the twenty
    /// functions that raise one. See the section header.
    fn throw(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(repr::WIDTH);
        match ctx.unwind {
            // In flight: park the value and raise the flag, then return, so the
            // caller's check is what carries it out. The pair is stored payload
            // first for no reason except that the tag is local 0 and reading them
            // in index order would be the one thing that needs a scratch.
            Some(unwind) => {
                f.body.push(ld(1));
                f.body.push(Ins::GlobalSet(unwind.payload));
                f.body.push(ld(0));
                f.body.push(Ins::GlobalSet(unwind.tag));
                f.body.push(ic(1));
                f.body.push(Ins::GlobalSet(unwind.flag));
            }
            // Nowhere for it to go. Say which of the three things this is, then
            // stop -- see [`super::runtime::FAULT_UNCAUGHT_THROW`].
            None => {
                record_uncaught_throw(&mut f.body);
                f.body.push(Ins::Unreachable);
            }
        }
        f
    }

    /// `__json_fail(message)`: throw the String at `message`.
    ///
    /// Not a SyntaxError or a TypeError object, because there is no `Error` and
    /// no prototype to hang one on. The thrown value is the message itself, which
    /// is the part a `catch (e)` would read anyway.
    fn json_fail_fn(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        box_string(&[ld(0)], &mut f.body);
        f.body.push(ctx.call(Js::Throw));
        f
    }

    // ---- the string buffer ---------------------------------------------------

    fn jb_new(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(0);
        let p = f.local(ValType::I32);
        let b = &mut f.body;
        b.push(ic(JB_HEADER));
        b.push(ctx.alloc());
        b.push(st(p));
        field_set(b, p, JB_LEN, |b| b.push(ic(0)));
        field_set(b, p, JB_CAP, |b| b.push(ic(JB_FIRST)));
        field_set(b, p, JB_DATA, |b| {
            b.push(ic(JB_FIRST));
            b.push(ctx.alloc());
        });
        b.push(ld(p));
        f
    }

    /// `__jb_room(p, n)`: make sure `n` more bytes fit, doubling and copying if
    /// they do not.
    fn jb_room(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(2);
        let need = f.local(ValType::I32);
        let cap = f.local(ValType::I32);
        let len = f.local(ValType::I32);
        let dst = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let b = &mut f.body;

        field_get(b, 0, JB_LEN);
        b.push(st(len));
        b.push(ld(len));
        b.push(ld(1));
        b.push(Ins::I32Add);
        b.push(st(need));
        field_get(b, 0, JB_CAP);
        b.push(st(cap));

        if_then(
            b,
            |b| lt(b, cap, need),
            |b| {
                while_loop(
                    b,
                    |b| lt(b, cap, need),
                    |b| {
                        b.push(ld(cap));
                        b.push(ic(2));
                        b.push(Ins::I32Mul);
                        b.push(st(cap));
                    },
                );
                b.push(ld(cap));
                b.push(ctx.alloc());
                b.push(st(dst));
                b.push(ic(0));
                b.push(st(i));
                while_loop(
                    b,
                    |b| lt(b, i, len),
                    |b| {
                        b.push(ld(dst));
                        b.push(ld(i));
                        b.push(Ins::I32Add);
                        field_get(b, 0, JB_DATA);
                        b.push(ld(i));
                        b.push(Ins::I32Add);
                        b.push(Ins::I32Load8U(0, 0));
                        b.push(Ins::I32Store8(0, 0));
                        bump(b, i, 1);
                    },
                );
                field_set(b, 0, JB_DATA, |b| b.push(ld(dst)));
                field_set(b, 0, JB_CAP, |b| b.push(ld(cap)));
            },
        );
        f
    }

    fn jb_byte(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(2);
        let b = &mut f.body;
        b.push(ld(0));
        b.push(ic(1));
        b.push(ctx.call(Js::JbRoom));
        field_get(b, 0, JB_DATA);
        field_get(b, 0, JB_LEN);
        b.push(Ins::I32Add);
        b.push(ld(1));
        b.push(Ins::I32Store8(0, 0));
        field_set(b, 0, JB_LEN, |b| {
            field_get(b, 0, JB_LEN);
            b.push(ic(1));
            b.push(Ins::I32Add);
        });
        f
    }

    /// `__jb_str(p, s)`: append the bytes of the string record `s`.
    fn jb_str(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(2);
        let n = f.local(ValType::I32);
        let dst = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let b = &mut f.body;

        b.push(ld(1));
        b.push(Ins::I32Load(ALIGN_WORD, 0));
        b.push(st(n));
        b.push(ld(0));
        b.push(ld(n));
        b.push(ctx.call(Js::JbRoom));
        field_get(b, 0, JB_DATA);
        field_get(b, 0, JB_LEN);
        b.push(Ins::I32Add);
        b.push(st(dst));
        b.push(ic(0));
        b.push(st(i));
        while_loop(
            b,
            |b| lt(b, i, n),
            |b| {
                b.push(ld(dst));
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(ld(1));
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(Ins::I32Load8U(0, STRING_HEADER as u32));
                b.push(Ins::I32Store8(0, 0));
                bump(b, i, 1);
            },
        );
        field_set(b, 0, JB_LEN, |b| {
            field_get(b, 0, JB_LEN);
            b.push(ld(n));
            b.push(Ins::I32Add);
        });
        f
    }

    /// `__jb_take(p) -> string record`. The buffer is not reusable afterwards and
    /// nothing tries: a bump heap has no free, so the copy is the honest cost of
    /// handing out a record whose header is a length and not a capacity.
    fn jb_take(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        let n = f.local(ValType::I32);
        let q = f.local(ValType::I32);
        let src = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let b = &mut f.body;

        field_get(b, 0, JB_LEN);
        b.push(st(n));
        b.push(ic(STRING_HEADER));
        b.push(ld(n));
        b.push(Ins::I32Add);
        b.push(ctx.alloc());
        b.push(st(q));
        b.push(ld(q));
        b.push(ld(n));
        b.push(Ins::I32Store(ALIGN_WORD, 0));
        field_get(b, 0, JB_DATA);
        b.push(st(src));
        b.push(ic(0));
        b.push(st(i));
        while_loop(
            b,
            |b| lt(b, i, n),
            |b| {
                b.push(ld(q));
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(ld(src));
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(Ins::I32Load8U(0, 0));
                b.push(Ins::I32Store8(0, STRING_HEADER as u32));
                bump(b, i, 1);
            },
        );
        b.push(ld(q));
        f
    }

    // ---- 25.5.2, stringify ---------------------------------------------------

    /// `__json_quote(s, buf)`: 25.5.2.2's QuoteJSONString, appended to `buf`.
    ///
    /// Table 74's seven characters get their two-byte escapes, everything else
    /// below U+0020 gets `\u00xx` in lowercase hexadecimal, and every other byte
    /// is copied through — which is what makes a multi-byte UTF-8 sequence come
    /// out as itself. The one arm of the spec that is missing is the lone
    /// surrogate, and the section header says why it has no reachable input.
    fn json_quote(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(2);
        let n = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let c = f.local(ValType::I32);
        let h = f.local(ValType::I32);
        let b = &mut f.body;
        let buf = 1;

        put(ctx, b, buf, 0x22);
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, 0));
        b.push(st(n));
        b.push(ic(0));
        b.push(st(i));
        while_loop(
            b,
            |b| lt(b, i, n),
            |b| {
                b.push(ld(0));
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(Ins::I32Load8U(0, STRING_HEADER as u32));
                b.push(st(c));
                // One `block` whose every arm leaves it, so that the last line is
                // the default and not a chain of negated conditions.
                b.push(Ins::Block(BlockType::Empty));
                for (byte, escape) in [
                    (0x22, 0x22),
                    (0x5c, 0x5c),
                    (0x08, b'b' as i32),
                    (0x09, b't' as i32),
                    (0x0a, b'n' as i32),
                    (0x0c, b'f' as i32),
                    (0x0d, b'r' as i32),
                ] {
                    b.push(ld(c));
                    b.push(ic(byte));
                    b.push(Ins::I32Eq);
                    b.push(Ins::If(BlockType::Empty));
                    put(ctx, b, buf, 0x5c);
                    put(ctx, b, buf, escape);
                    b.push(Ins::Br(1));
                    b.push(Ins::End);
                }
                b.push(ld(c));
                b.push(ic(0x20));
                b.push(Ins::I32LtS);
                b.push(Ins::If(BlockType::Empty));
                put(ctx, b, buf, 0x5c);
                put(ctx, b, buf, b'u' as i32);
                put(ctx, b, buf, b'0' as i32);
                put(ctx, b, buf, b'0' as i32);
                for divisor in [16, 1] {
                    b.push(ld(c));
                    b.push(ic(16));
                    if divisor == 16 {
                        b.push(Ins::I32DivS);
                    } else {
                        b.push(Ins::I32RemS);
                    }
                    b.push(st(h));
                    hex_digit(ctx, b, buf, h);
                }
                b.push(Ins::Br(1));
                b.push(Ins::End);
                b.push(ld(buf));
                b.push(ld(c));
                b.push(ctx.call(Js::JbByte));
                b.push(Ins::End);
                bump(b, i, 1);
            },
        );
        put(ctx, b, buf, 0x22);
        f
    }

    /// Append the lowercase hexadecimal digit for the value in local `h`.
    /// `h + '0'`, plus 39 more when `h > 9` — one multiply instead of a branch,
    /// because the instruction set has no `select` and an `if` here would be
    /// three blocks per escaped character.
    fn hex_digit(ctx: &JsonCtx, b: &mut Vec<Ins>, buf: u32, h: u32) {
        b.push(ld(buf));
        b.push(ld(h));
        b.push(ic(48));
        b.push(Ins::I32Add);
        b.push(ic(9));
        b.push(ld(h));
        b.push(Ins::I32LtS);
        b.push(ic(39));
        b.push(Ins::I32Mul);
        b.push(Ins::I32Add);
        b.push(ctx.call(Js::JbByte));
    }

    /// `__json_ser(value, buf, stack) -> wrote`: 25.5.2.2 SerializeJSONProperty,
    /// with no replacer, no gap and no `toJSON` — none of which this engine has a
    /// way to express, and each of which is refused at the entry point rather
    /// than ignored here.
    ///
    /// `0` means **omit**: `undefined` and a function value are not JSON, and
    /// 25.5.2.2 returns `undefined` for them, which the two callers read as "skip
    /// this property" and "the whole answer is `undefined`" respectively.
    ///
    /// # Dispatch order
    ///
    /// Number, then String, then the rest, which is [`super::repr`]'s documented
    /// order kept rather than departed from. It is worth saying why it is *not*
    /// departed from at a site whose dominant input is arguably an Object: the
    /// Object arm is reached once per object and the Number and String arms once
    /// per *scalar*, and a document has more scalars than objects. The two arms
    /// that were appended last are the two that cost nothing to reach late,
    /// because they end the walk.
    fn json_ser(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(4);
        let buf = 2;
        let stack = 3;
        let x = f.local(ValType::F64);
        let names = ctx.names;

        is_number(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        {
            let b = &mut f.body;
            unbox_number(0, b);
            b.push(st(x));
            // 25.5.2.2 step 10: a non-finite Number is `null`. `x != x` is the
            // NaN test and `|x| == inf` is the other two.
            b.push(ld(x));
            b.push(ld(x));
            b.push(Ins::F64Ne);
            b.push(ld(x));
            b.push(Ins::F64Abs);
            b.push(Ins::F64Const(f64::INFINITY));
            b.push(Ins::F64Eq);
            b.push(Ins::I32Or);
            b.push(Ins::If(BlockType::Empty));
            puts(ctx, b, buf, names.null);
            b.push(ic(1));
            b.push(Ins::Return);
            b.push(Ins::End);
            b.push(ld(buf));
            b.push(ld(x));
            b.push(ctx.cv(Cv::NumToString));
            b.push(ctx.call(Js::JbStr));
            b.push(ic(1));
            b.push(Ins::Return);
        }
        f.body.push(Ins::End);

        is_string(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        unbox_string(0, &mut f.body);
        f.body.push(ld(buf));
        f.body.push(ctx.call(Js::Quote));
        f.body.push(ic(1));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);

        is_bool(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        unbox_bool(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        puts(ctx, &mut f.body, buf, names.yes);
        f.body.push(ic(1));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);
        puts(ctx, &mut f.body, buf, names.no);
        f.body.push(ic(1));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);

        is_null(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        puts(ctx, &mut f.body, buf, names.null);
        f.body.push(ic(1));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);

        is_undefined(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        f.body.push(ic(0));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);

        is_object(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        unbox_object(0, &mut f.body);
        f.body.push(ld(buf));
        f.body.push(ld(stack));
        f.body.push(ctx.call(Js::SerObj));
        f.body.push(ic(1));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);

        is_function(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        f.body.push(ic(0));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);

        // Appended last, under `repr`'s dispatch-order rule: no type that
        // serialized before arrays existed pays a test for them.
        is_array(0, &mut f.body);
        f.body.push(Ins::If(BlockType::Empty));
        unbox_array(0, &mut f.body);
        f.body.push(ld(buf));
        f.body.push(ld(stack));
        f.body.push(ctx.call(Js::SerArr));
        f.body.push(ic(1));
        f.body.push(Ins::Return);
        f.body.push(Ins::End);

        // There is no ninth tag. Reaching here is a defect in this engine, not in
        // the script, which is why it is a trap and not an answer.
        f.body.push(Ins::Unreachable);
        f
    }

    /// `__json_ser_obj(o, buf, stack)`: 25.5.2.4 SerializeJSONObject, over an
    /// object with no prototype, in the record's own order — which is insertion
    /// order, which is 10.1.11.1's.
    fn json_ser_obj(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(3);
        let buf = 1;
        let stack = 2;
        let walk = f.local(ValType::I32);
        let node = f.local(ValType::I32);
        let first = f.local(ValType::I32);
        let len = f.local(ValType::I32);
        let entries = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let ep = f.local(ValType::I32);
        let tag = f.local(ValType::I32);
        let names = ctx.names;
        let b = &mut f.body;

        // 25.5.2.4 step 1: an object that contains itself is a TypeError. The
        // chain walked is the ancestors', so a value reached twice by two paths
        // is serialized twice, which is what the spec says and what a DAG needs.
        b.push(ld(stack));
        b.push(st(walk));
        while_loop(
            b,
            |b| b.push(ld(walk)),
            |b| {
                if_then(
                    b,
                    |b| {
                        b.push(ld(walk));
                        b.push(Ins::I32Load(ALIGN_WORD, CY_OBJ));
                        b.push(ld(0));
                        b.push(Ins::I32Eq);
                    },
                    |b| fail(ctx, b, names.cycle, Ret::None),
                );
                b.push(ld(walk));
                b.push(Ins::I32Load(ALIGN_WORD, CY_PARENT));
                b.push(st(walk));
            },
        );
        b.push(ic(CY_BYTES));
        b.push(ctx.alloc());
        b.push(st(node));
        b.push(ld(node));
        b.push(ld(stack));
        b.push(Ins::I32Store(ALIGN_WORD, CY_PARENT));
        b.push(ld(node));
        b.push(ld(0));
        b.push(Ins::I32Store(ALIGN_WORD, CY_OBJ));

        put(ctx, b, buf, b'{' as i32);
        b.push(ic(1));
        b.push(st(first));
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, OBJ_LEN));
        b.push(st(len));
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, OBJ_ENTRIES));
        b.push(st(entries));
        b.push(ic(0));
        b.push(st(i));
        while_loop(
            b,
            |b| lt(b, i, len),
            |b| {
                b.push(ld(entries));
                b.push(ld(i));
                b.push(ic(ENTRY_BYTES));
                b.push(Ins::I32Mul);
                b.push(Ins::I32Add);
                b.push(st(ep));
                b.push(ld(ep));
                b.push(Ins::I32Load(ALIGN_WORD, ENTRY_TAG));
                b.push(st(tag));
                // 25.5.2.4 step 5: a property whose value serializes to
                // `undefined` is left out entirely -- comma, key and all. The two
                // tags that do that are known here, so the test is exact and
                // nothing has to be written and taken back.
                if_then(
                    b,
                    |b| {
                        b.push(ld(tag));
                        b.push(ic(TAG_UNDEFINED));
                        b.push(Ins::I32Ne);
                        b.push(ld(tag));
                        b.push(ic(TAG_FUNCTION));
                        b.push(Ins::I32Ne);
                        b.push(Ins::I32And);
                    },
                    |b| {
                        if_then(
                            b,
                            |b| {
                                b.push(ld(first));
                                b.push(Ins::I32Eqz);
                            },
                            |b| put(ctx, b, buf, b',' as i32),
                        );
                        b.push(ld(ep));
                        b.push(Ins::I32Load(ALIGN_WORD, ENTRY_KEY));
                        b.push(ld(buf));
                        b.push(ctx.call(Js::Quote));
                        put(ctx, b, buf, b':' as i32);
                        b.push(ld(tag));
                        b.push(ld(ep));
                        b.push(Ins::I64Load(ALIGN_WORD, ENTRY_PAYLOAD));
                        b.push(ld(buf));
                        b.push(ld(node));
                        b.push(ctx.call(Js::Ser));
                        b.push(Ins::Drop);
                        // A nested value threw. Leave now, or the loop writes a
                        // comma and a key for a document that will never exist.
                        check(ctx, b, Ret::None);
                        b.push(ic(0));
                        b.push(st(first));
                    },
                );
                bump(b, i, 1);
            },
        );
        put(ctx, b, buf, b'}' as i32);
        f
    }

    /// `__json_ser_arr(a, buf, stack)`: 25.5.2.5 SerializeJSONArray.
    ///
    /// The sibling of [`json_ser_obj`] and deliberately its near-copy, down to
    /// the cycle node it pushes -- an array that contains itself is the same
    /// TypeError as an object that does, and the chain walked is the
    /// ancestors' for the same reason.
    ///
    /// # One step differs, and it is the step people get wrong
    ///
    /// 25.5.2.4 step 5 *omits* an object property whose value serializes to
    /// nothing. 25.5.2.5 step 8 **writes `null`** for the same value in an
    /// array, because an array's indices are positional and dropping one would
    /// renumber every element after it. So `[undefined, 1]` is `[null,1]`
    /// while `{a: undefined, b: 1}` is `{"b":1}`, and the two arms of this file
    /// disagree on purpose.
    fn json_ser_arr(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(3);
        let buf = 1;
        let stack = 2;
        let walk = f.local(ValType::I32);
        let node = f.local(ValType::I32);
        let len = f.local(ValType::I32);
        let elems = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let ep = f.local(ValType::I32);
        let tag = f.local(ValType::I32);
        let keep = f.local(ValType::I32);
        let names = ctx.names;
        let b = &mut f.body;

        b.push(ld(stack));
        b.push(st(walk));
        while_loop(
            b,
            |b| b.push(ld(walk)),
            |b| {
                if_then(
                    b,
                    |b| {
                        b.push(ld(walk));
                        b.push(Ins::I32Load(ALIGN_WORD, CY_OBJ));
                        b.push(ld(0));
                        b.push(Ins::I32Eq);
                    },
                    |b| fail(ctx, b, names.cycle, Ret::None),
                );
                b.push(ld(walk));
                b.push(Ins::I32Load(ALIGN_WORD, CY_PARENT));
                b.push(st(walk));
            },
        );
        b.push(ic(CY_BYTES));
        b.push(ctx.alloc());
        b.push(st(node));
        b.push(ld(node));
        b.push(ld(stack));
        b.push(Ins::I32Store(ALIGN_WORD, CY_PARENT));
        b.push(ld(node));
        b.push(ld(0));
        b.push(Ins::I32Store(ALIGN_WORD, CY_OBJ));

        put(ctx, b, buf, b'[' as i32);
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
        b.push(st(len));
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
        b.push(st(elems));
        b.push(ic(0));
        b.push(st(i));
        while_loop(
            b,
            |b| lt(b, i, len),
            |b| {
                if_then(
                    b,
                    |b| {
                        b.push(ld(i));
                        b.push(ic(0));
                        b.push(Ins::I32Ne);
                    },
                    |b| put(ctx, b, buf, b',' as i32),
                );
                b.push(ld(elems));
                b.push(ld(i));
                b.push(ic(ELEM_BYTES));
                b.push(Ins::I32Mul);
                b.push(Ins::I32Add);
                b.push(st(ep));
                b.push(ld(ep));
                b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
                b.push(st(tag));
                // Step 8: the two tags `__json_ser` answers with "wrote
                // nothing" become `null` here rather than disappearing. The
                // test is exact -- the same two tags `json_ser_obj` uses to
                // decide to *omit* -- so nothing has to be written and taken
                // back.
                //
                // Written as two tests of one local rather than as one
                // if/else, because this instruction set has no `else`: see
                // `runtime::obj_new`, which says the same thing about a zero
                // pointer.
                b.push(ld(tag));
                b.push(ic(TAG_UNDEFINED));
                b.push(Ins::I32Ne);
                b.push(ld(tag));
                b.push(ic(TAG_FUNCTION));
                b.push(Ins::I32Ne);
                b.push(Ins::I32And);
                b.push(st(keep));
                if_then(
                    b,
                    |b| b.push(ld(keep)),
                    |b| {
                        b.push(ld(tag));
                        b.push(ld(ep));
                        b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
                        b.push(ld(buf));
                        b.push(ld(node));
                        b.push(ctx.call(Js::Ser));
                        b.push(Ins::Drop);
                        check(ctx, b, Ret::None);
                    },
                );
                if_then(
                    b,
                    |b| {
                        b.push(ld(keep));
                        b.push(Ins::I32Eqz);
                    },
                    |b| puts(ctx, b, buf, names.null),
                );
                bump(b, i, 1);
            },
        );
        put(ctx, b, buf, b']' as i32);
        f
    }

    /// `__json_stringify(value, replacer, space) -> value`: 25.5.2.
    ///
    /// # Why it declares three parameters
    ///
    /// So that `JSON.stringify(o, null, 2)` cannot be silently answered with
    /// unindented text. The adapter forwards what the callee declares, so a
    /// one-parameter `__json_stringify` would make the second and third arguments
    /// evaluate and vanish -- a wrong answer that looks like a right one, which is
    /// the failure mode this whole stack refuses. Declaring them costs the
    /// program's uniform arity a floor of three, and nothing at all when the
    /// program already has a three-parameter function, which the acceptance
    /// target does.
    fn json_stringify(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(3 * repr::WIDTH);
        let buf = f.local(ValType::I32);
        let wrote = f.local(ValType::I32);
        let names = ctx.names;

        for base in [repr::WIDTH, 2 * repr::WIDTH] {
            is_nullish(base, &mut f.body);
            f.body.push(Ins::I32Eqz);
            f.body.push(Ins::If(BlockType::Empty));
            fail(ctx, &mut f.body, names.replacer, Ret::Value);
            f.body.push(Ins::End);
        }

        let b = &mut f.body;
        b.push(ctx.call(Js::JbNew));
        b.push(st(buf));
        b.push(ld(0));
        b.push(ld(1));
        b.push(ld(buf));
        b.push(ic(0));
        b.push(ctx.call(Js::Ser));
        b.push(st(wrote));
        check(ctx, b, Ret::Value);
        b.push(ld(wrote));
        b.push(Ins::I32Eqz);
        b.push(Ins::If(BlockType::Empty));
        const_undefined(b);
        b.push(Ins::Return);
        b.push(Ins::End);
        box_string(&[ld(buf), ctx.call(Js::JbTake)], b);
        f
    }

    // ---- 25.5.1, parse -------------------------------------------------------

    /// `__jp_at(state) -> byte, or -1 at the end`.
    ///
    /// `-1` and not a separate "at end" call, because every caller asks both
    /// questions at once and a byte can never be negative.
    fn jp_at() -> FnBuild {
        let mut f = FnBuild::new(1);
        let src = f.local(ValType::I32);
        let b = &mut f.body;
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, PS_SRC));
        b.push(st(src));
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, PS_POS));
        b.push(ld(src));
        b.push(Ins::I32Load(ALIGN_WORD, 0));
        b.push(Ins::I32GeU);
        b.push(Ins::If(BlockType::Empty));
        b.push(ic(-1));
        b.push(Ins::Return);
        b.push(Ins::End);
        b.push(ld(src));
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, PS_POS));
        b.push(Ins::I32Add);
        b.push(Ins::I32Load8U(0, STRING_HEADER as u32));
        f
    }

    /// `__jp_ws(state)`: JSONWhiteSpace, which is **four** characters — tab, line
    /// feed, carriage return and space. Not `StrWhiteSpace`: 25.5.1's grammar is
    /// its own, and `__skip_ws` next door would accept NBSP, the Zs category and
    /// the line separators, none of which JSON has.
    fn jp_ws(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        let c = f.local(ValType::I32);
        let b = &mut f.body;
        while_loop(
            b,
            |b| {
                at_byte(ctx, b, 0);
                b.push(st(c));
                for (index, ch) in [0x20, 0x09, 0x0a, 0x0d].into_iter().enumerate() {
                    b.push(ld(c));
                    b.push(ic(ch));
                    b.push(Ins::I32Eq);
                    if index > 0 {
                        b.push(Ins::I32Or);
                    }
                }
            },
            |b| advance(b, 0, 1),
        );
        f
    }

    /// `__jp_lit(state, word) -> matched`: `true`, `false` and `null`, whole.
    ///
    /// Whole and not first-character-plus-skip, because `nul` and `nulll` are
    /// both refusals and only a full compare with a bounds check catches the
    /// first.
    fn jp_lit() -> FnBuild {
        let mut f = FnBuild::new(2);
        let n = f.local(ValType::I32);
        let src = f.local(ValType::I32);
        let pos = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let b = &mut f.body;

        b.push(ld(1));
        b.push(Ins::I32Load(ALIGN_WORD, 0));
        b.push(st(n));
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, PS_SRC));
        b.push(st(src));
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, PS_POS));
        b.push(st(pos));
        b.push(ld(src));
        b.push(Ins::I32Load(ALIGN_WORD, 0));
        b.push(ld(pos));
        b.push(ld(n));
        b.push(Ins::I32Add);
        b.push(Ins::I32LtS);
        b.push(Ins::If(BlockType::Empty));
        b.push(ic(0));
        b.push(Ins::Return);
        b.push(Ins::End);
        b.push(ic(0));
        b.push(st(i));
        while_loop(
            b,
            |b| lt(b, i, n),
            |b| {
                b.push(ld(src));
                b.push(ld(pos));
                b.push(Ins::I32Add);
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(Ins::I32Load8U(0, STRING_HEADER as u32));
                b.push(ld(1));
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(Ins::I32Load8U(0, STRING_HEADER as u32));
                b.push(Ins::I32Ne);
                b.push(Ins::If(BlockType::Empty));
                b.push(ic(0));
                b.push(Ins::Return);
                b.push(Ins::End);
                bump(b, i, 1);
            },
        );
        b.push(ld(0));
        b.push(ld(pos));
        b.push(ld(n));
        b.push(Ins::I32Add);
        b.push(Ins::I32Store(ALIGN_WORD, PS_POS));
        b.push(ic(1));
        f
    }

    /// `__jp_hex4(state) -> code unit`: exactly four hexadecimal digits, in
    /// either case. Three is a refusal, and so is `\uZZZZ`.
    fn jp_hex4(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        let v = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let c = f.local(ValType::I32);
        let d = f.local(ValType::I32);
        let names = ctx.names;
        let b = &mut f.body;

        b.push(ic(0));
        b.push(st(v));
        b.push(ic(0));
        b.push(st(i));
        while_loop(
            b,
            |b| {
                b.push(ld(i));
                b.push(ic(4));
                b.push(Ins::I32LtS);
            },
            |b| {
                at_byte(ctx, b, 0);
                b.push(st(c));
                b.push(ic(-1));
                b.push(st(d));
                for (lo, hi, base) in [(47, 58, 48), (96, 103, 87), (64, 71, 55)] {
                    if_then(
                        b,
                        |b| {
                            b.push(ic(lo));
                            b.push(ld(c));
                            b.push(Ins::I32LtS);
                            b.push(ld(c));
                            b.push(ic(hi));
                            b.push(Ins::I32LtS);
                            b.push(Ins::I32And);
                        },
                        |b| {
                            b.push(ld(c));
                            b.push(ic(base));
                            b.push(Ins::I32Sub);
                            b.push(st(d));
                        },
                    );
                }
                if_then(
                    b,
                    |b| {
                        b.push(ld(d));
                        b.push(ic(0));
                        b.push(Ins::I32LtS);
                    },
                    |b| fail(ctx, b, names.syntax, Ret::I32),
                );
                b.push(ld(v));
                b.push(ic(16));
                b.push(Ins::I32Mul);
                b.push(ld(d));
                b.push(Ins::I32Add);
                b.push(st(v));
                advance(b, 0, 1);
                bump(b, i, 1);
            },
        );
        b.push(ld(v));
        f
    }

    /// `__jp_utf8(buf, cp)`: one code point, UTF-8 encoded.
    ///
    /// Division and remainder rather than shift and mask, because
    /// [`super::repr`]'s instruction set has neither `i32.shr_u` nor `i32.and`
    /// against a mask it could reach — the same constraint that chose 16-bit
    /// limbs for the bignum. Every operand here is non-negative and under
    /// `2^21`, where `i32.div_s` is the unsigned division.
    fn jp_utf8(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(2);
        let b = &mut f.body;
        let buf = 0;
        let cp = 1;

        // The four ranges, in order, each ending the function. A byte is
        // `lead + (cp / scale) % 64`, and `six` says whether the remainder is
        // taken at all: a **continuation** byte keeps six bits, a **leading** byte
        // keeps whatever the range test already bounded. Taking the remainder on
        // a leading byte is a no-op for three of the four arms -- the quotient is
        // already under 64 -- and is wrong for the fourth, where the whole code
        // point is the byte. `"\u0041"` decoded to U+0001 until this was two
        // cases instead of one.
        /// One emitted byte: `lead + (cp / scale) % 64`, with `six` saying whether
        /// the remainder is taken.
        type Utf8Byte = (i32, i32, bool);
        let plan: [(i32, &[Utf8Byte]); 4] = [
            (0x80, &[(0x00, 1, false)]),
            (0x800, &[(0xc0, 64, false), (0x80, 1, true)]),
            (
                0x10000,
                &[(0xe0, 4096, false), (0x80, 64, true), (0x80, 1, true)],
            ),
            (
                i32::MAX,
                &[
                    (0xf0, 262_144, false),
                    (0x80, 4096, true),
                    (0x80, 64, true),
                    (0x80, 1, true),
                ],
            ),
        ];
        for (limit, bytes) in plan {
            let last = limit == i32::MAX;
            if !last {
                b.push(ld(cp));
                b.push(ic(limit));
                b.push(Ins::I32LtS);
                b.push(Ins::If(BlockType::Empty));
            }
            for (lead, scale, six) in bytes {
                b.push(ld(buf));
                b.push(ic(*lead));
                b.push(ld(cp));
                if *scale > 1 {
                    b.push(ic(*scale));
                    b.push(Ins::I32DivS);
                }
                if *six {
                    b.push(ic(64));
                    b.push(Ins::I32RemS);
                }
                b.push(Ins::I32Add);
                b.push(ctx.call(Js::JbByte));
            }
            if !last {
                b.push(Ins::Return);
                b.push(Ins::End);
            }
        }
        f
    }

    /// `__json_pstr(state) -> string record`: 25.5.1's JSONString, whole.
    ///
    /// The three refusals that make this the JSON grammar and not JavaScript's:
    /// a raw character below U+0020 is not allowed in a string, `\x` and every
    /// other escape outside the list is not an escape, and a `\u` escape naming
    /// half a surrogate pair has no representation here.
    fn json_pstr(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        let buf = f.local(ValType::I32);
        let c = f.local(ValType::I32);
        let e = f.local(ValType::I32);
        let u = f.local(ValType::I32);
        let v = f.local(ValType::I32);
        let names = ctx.names;
        let b = &mut f.body;

        advance(b, 0, 1);
        b.push(ctx.call(Js::JbNew));
        b.push(st(buf));
        // Depth 0 inside the body is this loop; `br 0` is another character.
        b.push(Ins::Loop(BlockType::Empty));
        at_byte(ctx, b, 0);
        b.push(st(c));

        b.push(ld(c));
        b.push(ic(-1));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        fail(ctx, b, names.eof, Ret::I32);
        b.push(Ins::End);

        b.push(ld(c));
        b.push(ic(0x22));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        advance(b, 0, 1);
        b.push(ld(buf));
        b.push(ctx.call(Js::JbTake));
        b.push(Ins::Return);
        b.push(Ins::End);

        b.push(ld(c));
        b.push(ic(0x5c));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        {
            // Inside: 0 = this `if`, 1 = the loop.
            advance(b, 0, 1);
            at_byte(ctx, b, 0);
            b.push(st(e));
            // 0 = this block, 1 = the `if`, 2 = the loop.
            b.push(Ins::Block(BlockType::Empty));
            for (spelling, byte) in [
                (0x22, 0x22),
                (0x5c, 0x5c),
                (0x2f, 0x2f),
                (b'b' as i32, 0x08),
                (b'f' as i32, 0x0c),
                (b'n' as i32, 0x0a),
                (b'r' as i32, 0x0d),
                (b't' as i32, 0x09),
            ] {
                b.push(ld(e));
                b.push(ic(spelling));
                b.push(Ins::I32Eq);
                b.push(Ins::If(BlockType::Empty));
                put(ctx, b, buf, byte);
                advance(b, 0, 1);
                b.push(Ins::Br(1));
                b.push(Ins::End);
            }
            b.push(ld(e));
            b.push(ic(b'u' as i32));
            b.push(Ins::I32Eq);
            b.push(Ins::If(BlockType::Empty));
            {
                // Inside: 0 = this `if`, 1 = the block, 2 = the outer `if`.
                advance(b, 0, 1);
                b.push(ld(0));
                b.push(ctx.call(Js::PHex4));
                b.push(st(u));
                check(ctx, b, Ret::I32);
                b.push(Ins::Block(BlockType::Empty));
                {
                    // 0 = this block, 1 = the `\u` if, 2 = the escape block.
                    b.push(Ins::Block(BlockType::Empty));
                    between(b, u, 0xd7ff, 0xdc00);
                    b.push(Ins::I32Eqz);
                    b.push(Ins::BrIf(0));
                    // A leading surrogate: only a `\uDC00`-`\uDFFF` right behind
                    // it is a character. Anything else names half of one, and a
                    // guest String is UTF-8, so there is nothing to build.
                    at_byte(ctx, b, 0);
                    b.push(ic(0x5c));
                    b.push(Ins::I32Ne);
                    b.push(Ins::If(BlockType::Empty));
                    fail(ctx, b, names.surrogate, Ret::I32);
                    b.push(Ins::End);
                    advance(b, 0, 1);
                    at_byte(ctx, b, 0);
                    b.push(ic(b'u' as i32));
                    b.push(Ins::I32Ne);
                    b.push(Ins::If(BlockType::Empty));
                    fail(ctx, b, names.surrogate, Ret::I32);
                    b.push(Ins::End);
                    advance(b, 0, 1);
                    b.push(ld(0));
                    b.push(ctx.call(Js::PHex4));
                    b.push(st(v));
                    check(ctx, b, Ret::I32);
                    between(b, v, 0xdbff, 0xe000);
                    b.push(Ins::I32Eqz);
                    b.push(Ins::If(BlockType::Empty));
                    fail(ctx, b, names.surrogate, Ret::I32);
                    b.push(Ins::End);
                    // 11.1.3 UTF16SurrogatePairToCodePoint.
                    b.push(ld(buf));
                    b.push(ic(0x10000));
                    b.push(ld(u));
                    b.push(ic(0xd800));
                    b.push(Ins::I32Sub);
                    b.push(ic(1024));
                    b.push(Ins::I32Mul);
                    b.push(Ins::I32Add);
                    b.push(ld(v));
                    b.push(ic(0xdc00));
                    b.push(Ins::I32Sub);
                    b.push(Ins::I32Add);
                    b.push(ctx.call(Js::PUtf8));
                    b.push(Ins::Br(1));
                    b.push(Ins::End);
                    // A trailing surrogate with nothing in front of it is the
                    // same refusal from the other side.
                    between(b, u, 0xdbff, 0xe000);
                    b.push(Ins::If(BlockType::Empty));
                    fail(ctx, b, names.surrogate, Ret::I32);
                    b.push(Ins::End);
                    b.push(ld(buf));
                    b.push(ld(u));
                    b.push(ctx.call(Js::PUtf8));
                }
                b.push(Ins::End);
                b.push(Ins::Br(1));
            }
            b.push(Ins::End);
            fail(ctx, b, names.syntax, Ret::I32);
            b.push(Ins::End);
            b.push(Ins::Br(1));
        }
        b.push(Ins::End);

        b.push(ld(c));
        b.push(ic(0x20));
        b.push(Ins::I32LtS);
        b.push(Ins::If(BlockType::Empty));
        fail(ctx, b, names.syntax, Ret::I32);
        b.push(Ins::End);

        b.push(ld(buf));
        b.push(ld(c));
        b.push(ctx.call(Js::JbByte));
        advance(b, 0, 1);
        b.push(Ins::Br(0));
        b.push(Ins::End);
        // The loop leaves only by `return`.
        b.push(Ins::Unreachable);
        f
    }

    /// `__json_pnum(state) -> f64`: 25.5.1's JSONNumber.
    ///
    /// The grammar is validated here and the *value* is
    /// [`Cv::StrToNum`]'s, over the exact bytes just accepted. That split is the
    /// whole design: `StringToNumber` accepts `0x10`, `Infinity`, a leading `+`
    /// and a bare `.5`, none of which JSON has, so handing it the text
    /// unvalidated would accept them too; and re-deriving the double here would
    /// be a second, less exact answer to a question 7.1.4.1 already answers
    /// correctly rounded.
    fn json_pnum(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        let start = f.local(ValType::I32);
        let src = f.local(ValType::I32);
        let n = f.local(ValType::I32);
        let q = f.local(ValType::I32);
        let i = f.local(ValType::I32);
        let c = f.local(ValType::I32);
        let names = ctx.names;
        let b = &mut f.body;

        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, PS_SRC));
        b.push(st(src));
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, PS_POS));
        b.push(st(start));

        if_then(
            b,
            |b| {
                at_byte(ctx, b, 0);
                b.push(ic(0x2d));
                b.push(Ins::I32Eq);
            },
            |b| advance(b, 0, 1),
        );

        // DecimalIntegerLiteral: `0`, or a non-zero digit and more. `01` is not a
        // JSON number, which is why the zero is its own arm and not a digit run.
        at_byte(ctx, b, 0);
        b.push(st(c));
        b.push(Ins::Block(BlockType::Empty));
        b.push(ld(c));
        b.push(ic(0x30));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        advance(b, 0, 1);
        b.push(Ins::Br(1));
        b.push(Ins::End);
        digit(b, c);
        b.push(Ins::I32Eqz);
        b.push(Ins::If(BlockType::Empty));
        fail(ctx, b, names.syntax, Ret::F64);
        b.push(Ins::End);
        while_loop(b, |b| digit_at(ctx, b, 0, c), |b| advance(b, 0, 1));
        b.push(Ins::End);

        // A fraction, if there is a point, and at least one digit after it.
        if_then(
            b,
            |b| {
                at_byte(ctx, b, 0);
                b.push(ic(0x2e));
                b.push(Ins::I32Eq);
            },
            |b| {
                advance(b, 0, 1);
                digit_at(ctx, b, 0, c);
                b.push(Ins::I32Eqz);
                b.push(Ins::If(BlockType::Empty));
                fail(ctx, b, names.syntax, Ret::F64);
                b.push(Ins::End);
                while_loop(b, |b| digit_at(ctx, b, 0, c), |b| advance(b, 0, 1));
            },
        );

        // An exponent, if there is an `e`, with an optional sign and at least one
        // digit -- `1e` and `1e+` are both refusals.
        at_byte(ctx, b, 0);
        b.push(st(c));
        if_then(
            b,
            |b| {
                b.push(ld(c));
                b.push(ic(0x65));
                b.push(Ins::I32Eq);
                b.push(ld(c));
                b.push(ic(0x45));
                b.push(Ins::I32Eq);
                b.push(Ins::I32Or);
            },
            |b| {
                advance(b, 0, 1);
                at_byte(ctx, b, 0);
                b.push(st(c));
                if_then(
                    b,
                    |b| {
                        b.push(ld(c));
                        b.push(ic(0x2b));
                        b.push(Ins::I32Eq);
                        b.push(ld(c));
                        b.push(ic(0x2d));
                        b.push(Ins::I32Eq);
                        b.push(Ins::I32Or);
                    },
                    |b| advance(b, 0, 1),
                );
                digit_at(ctx, b, 0, c);
                b.push(Ins::I32Eqz);
                b.push(Ins::If(BlockType::Empty));
                fail(ctx, b, names.syntax, Ret::F64);
                b.push(Ins::End);
                while_loop(b, |b| digit_at(ctx, b, 0, c), |b| advance(b, 0, 1));
            },
        );

        // The accepted text, as a record `__str_to_num` can read.
        b.push(ld(0));
        b.push(Ins::I32Load(ALIGN_WORD, PS_POS));
        b.push(ld(start));
        b.push(Ins::I32Sub);
        b.push(st(n));
        b.push(ic(STRING_HEADER));
        b.push(ld(n));
        b.push(Ins::I32Add);
        b.push(ctx.alloc());
        b.push(st(q));
        b.push(ld(q));
        b.push(ld(n));
        b.push(Ins::I32Store(ALIGN_WORD, 0));
        b.push(ic(0));
        b.push(st(i));
        while_loop(
            b,
            |b| lt(b, i, n),
            |b| {
                b.push(ld(q));
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(ld(src));
                b.push(ld(start));
                b.push(Ins::I32Add);
                b.push(ld(i));
                b.push(Ins::I32Add);
                b.push(Ins::I32Load8U(0, STRING_HEADER as u32));
                b.push(Ins::I32Store8(0, STRING_HEADER as u32));
                bump(b, i, 1);
            },
        );
        b.push(ld(q));
        b.push(ctx.cv(Cv::StrToNum));
        f
    }

    /// `__json_pobj(state) -> value`: 25.5.1's JSONObject.
    ///
    /// A repeated key is the later value at the earlier position, which is what
    /// `__obj_set` does anyway (10.1.9.2 overwrites in place) and what
    /// `JSON.parse` specifies.
    fn json_pobj(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        let o = f.local(ValType::I32);
        let k = f.local(ValType::I32);
        let c = f.local(ValType::I32);
        let v = f.value_local();
        let names = ctx.names;
        let b = &mut f.body;

        advance(b, 0, 1);
        b.push(ic(0));
        b.push(ctx.rt(Rt::ObjNew));
        b.push(st(o));
        skip_ws_at(ctx, b, 0);
        at_byte(ctx, b, 0);
        b.push(ic(0x7d));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        advance(b, 0, 1);
        box_object(&[ld(o)], b);
        b.push(Ins::Return);
        b.push(Ins::End);

        b.push(Ins::Loop(BlockType::Empty));
        skip_ws_at(ctx, b, 0);
        at_byte(ctx, b, 0);
        b.push(ic(0x22));
        b.push(Ins::I32Ne);
        b.push(Ins::If(BlockType::Empty));
        fail(ctx, b, names.syntax, Ret::Value);
        b.push(Ins::End);
        b.push(ld(0));
        b.push(ctx.call(Js::PStr));
        b.push(st(k));
        check(ctx, b, Ret::Value);
        skip_ws_at(ctx, b, 0);
        at_byte(ctx, b, 0);
        b.push(ic(0x3a));
        b.push(Ins::I32Ne);
        b.push(Ins::If(BlockType::Empty));
        fail(ctx, b, names.syntax, Ret::Value);
        b.push(Ins::End);
        advance(b, 0, 1);
        skip_ws_at(ctx, b, 0);
        // The member's value is parsed into a local *first*, and only then is
        // `__obj_set(receiver, key, value)` assembled. Parsing it inline as the
        // third argument would be shorter, and would leave the receiver and the
        // key stranded on the stack with a throw in flight between them.
        b.push(ld(0));
        b.push(ctx.call(Js::PVal));
        store_local(v, b);
        check(ctx, b, Ret::Value);
        box_object(&[ld(o)], b);
        b.push(ld(k));
        load_local(v, b);
        b.push(ctx.rt(Rt::ObjSet));
        skip_ws_at(ctx, b, 0);
        at_byte(ctx, b, 0);
        b.push(st(c));
        b.push(ld(c));
        b.push(ic(0x2c));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        advance(b, 0, 1);
        b.push(Ins::Br(1));
        b.push(Ins::End);
        b.push(ld(c));
        b.push(ic(0x7d));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        advance(b, 0, 1);
        box_object(&[ld(o)], b);
        b.push(Ins::Return);
        b.push(Ins::End);
        fail(ctx, b, names.syntax, Ret::Value);
        b.push(Ins::End);
        b.push(Ins::Unreachable);
        f
    }

    /// `__json_parr(state) -> value`: 25.5.1's JSONArray.
    ///
    /// The sibling of [`json_pobj`] with the keys taken out, and the same two
    /// shapes: the empty-array early exit before the loop, so `[]` never looks
    /// for an element, and one loop that reads a value then demands either a
    /// `,` or a `]`.
    ///
    /// `__arr_new(0)` rather than a guessed capacity: the length is not known
    /// until the text is read, and guessing would allocate for arrays that do
    /// not need it. `__arr_push` owns the growth, so this function does not
    /// have a second opinion about capacity.
    ///
    /// The element is parsed into a local before it is pushed, for the reason
    /// [`json_pobj`] gives about its member: pushing it inline would leave the
    /// array pointer stranded on the stack with a throw in flight underneath
    /// it.
    fn json_parr(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        let a = f.local(ValType::I32);
        let c = f.local(ValType::I32);
        let v = f.value_local();
        let names = ctx.names;
        let b = &mut f.body;

        advance(b, 0, 1);
        b.push(ic(0));
        b.push(ctx.ar(Ar::New));
        b.push(st(a));
        skip_ws_at(ctx, b, 0);
        at_byte(ctx, b, 0);
        b.push(ic(0x5d));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        advance(b, 0, 1);
        box_array(&[ld(a)], b);
        b.push(Ins::Return);
        b.push(Ins::End);

        b.push(Ins::Loop(BlockType::Empty));
        skip_ws_at(ctx, b, 0);
        b.push(ld(0));
        b.push(ctx.call(Js::PVal));
        store_local(v, b);
        check(ctx, b, Ret::Value);
        b.push(ld(a));
        load_local(v, b);
        b.push(ctx.ar(Ar::Push));
        skip_ws_at(ctx, b, 0);
        at_byte(ctx, b, 0);
        b.push(st(c));
        b.push(ld(c));
        b.push(ic(0x2c));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        advance(b, 0, 1);
        b.push(Ins::Br(1));
        b.push(Ins::End);
        b.push(ld(c));
        b.push(ic(0x5d));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        advance(b, 0, 1);
        box_array(&[ld(a)], b);
        b.push(Ins::Return);
        b.push(Ins::End);
        fail(ctx, b, names.syntax, Ret::Value);
        b.push(Ins::End);
        b.push(Ins::Unreachable);
        f
    }

    /// `__json_pval(state) -> value`: 25.5.1's JSONValue, dispatched on one byte.
    ///
    /// The `[` arm used to be the one refusal here that was about this engine
    /// rather than about the text -- `[1,2]` is perfectly good JSON and there
    /// was no Array to build it into. The Array milestone landed the type, so
    /// it parses, and `JsonNames::array` went with it.
    fn json_pval(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(1);
        let c = f.local(ValType::I32);
        let names = ctx.names;
        let b = &mut f.body;

        at_byte(ctx, b, 0);
        b.push(st(c));

        b.push(ld(c));
        b.push(ic(0x7b));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        b.push(ld(0));
        b.push(ctx.call(Js::PObj));
        b.push(Ins::Return);
        b.push(Ins::End);

        b.push(ld(c));
        b.push(ic(0x5b));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        b.push(ld(0));
        b.push(ctx.call(Js::PArr));
        b.push(Ins::Return);
        b.push(Ins::End);

        b.push(ld(c));
        b.push(ic(0x22));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        box_string(&[ld(0), ctx.call(Js::PStr)], b);
        b.push(Ins::Return);
        b.push(Ins::End);

        for (lead, word) in [
            (b't' as i32, names.yes),
            (b'f' as i32, names.no),
            (b'n' as i32, names.null),
        ] {
            b.push(ld(c));
            b.push(ic(lead));
            b.push(Ins::I32Eq);
            b.push(Ins::If(BlockType::Empty));
            b.push(ld(0));
            b.push(ic(word));
            b.push(ctx.call(Js::PLit));
            b.push(Ins::If(BlockType::Empty));
            if word == names.null {
                const_null(b);
            } else {
                box_bool(&[ic(i32::from(word == names.yes))], b);
            }
            b.push(Ins::Return);
            b.push(Ins::End);
            fail(ctx, b, names.syntax, Ret::Value);
            b.push(Ins::End);
        }

        b.push(ld(c));
        b.push(ic(0x2d));
        b.push(Ins::I32Eq);
        digit(b, c);
        b.push(Ins::I32Or);
        b.push(Ins::If(BlockType::Empty));
        box_number(&[ld(0), ctx.call(Js::PNum)], b);
        b.push(Ins::Return);
        b.push(Ins::End);

        b.push(ld(c));
        b.push(ic(-1));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        fail(ctx, b, names.eof, Ret::Value);
        b.push(Ins::End);

        fail(ctx, b, names.syntax, Ret::Value);
        f
    }

    /// `__json_parse(text, reviver) -> value`: 25.5.1.
    ///
    /// The argument runs through `__to_string` first, which is 25.5.1 step 1's
    /// `ToString(text)` and is why `JSON.parse(1)` reads the text `"1"`.
    ///
    /// The reviver is declared and refused for the reason
    /// [`json_stringify`] gives for the replacer.
    fn json_parse(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(2 * repr::WIDTH);
        let src = f.local(ValType::I32);
        let state = f.local(ValType::I32);
        let out = f.value_local();
        let names = ctx.names;

        is_nullish(repr::WIDTH, &mut f.body);
        f.body.push(Ins::I32Eqz);
        f.body.push(Ins::If(BlockType::Empty));
        fail(ctx, &mut f.body, names.reviver, Ret::Value);
        f.body.push(Ins::End);

        let b = &mut f.body;
        b.push(ld(0));
        b.push(ld(1));
        b.push(ctx.rt(Rt::ToStr));
        b.push(st(src));
        b.push(ic(PS_BYTES));
        b.push(ctx.alloc());
        b.push(st(state));
        b.push(ld(state));
        b.push(ld(src));
        b.push(Ins::I32Store(ALIGN_WORD, PS_SRC));
        b.push(ld(state));
        b.push(ic(0));
        b.push(Ins::I32Store(ALIGN_WORD, PS_POS));
        skip_ws_at(ctx, b, state);
        b.push(ld(state));
        b.push(ctx.call(Js::PVal));
        store_local(out, b);
        check(ctx, b, Ret::Value);
        skip_ws_at(ctx, b, state);
        // 25.5.1: the text is *one* JSONValue. `1 2` and `{} {}` are refusals,
        // and so is a comment, which has no production at all.
        b.push(ld(state));
        b.push(ctx.call(Js::PAt));
        b.push(ic(-1));
        b.push(Ins::I32Ne);
        b.push(Ins::If(BlockType::Empty));
        fail(ctx, b, names.syntax, Ret::Value);
        b.push(Ins::End);
        load_local(out, b);
        f
    }

    /// `__json_ns(stringify_element, parse_element) -> object record`.
    ///
    /// The whole of `JSON`. Two `__fn_new` records and two `__obj_set` calls --
    /// the same three runtime functions a script writing
    /// `const JSON = { stringify: function () {}, parse: function () {} };`
    /// would reach, which is the claim this file's header makes and this function
    /// is where it is either true or not.
    fn json_ns(ctx: &JsonCtx) -> FnBuild {
        let mut f = FnBuild::new(2);
        let o = f.local(ValType::I32);
        let names = ctx.names;
        let b = &mut f.body;

        b.push(ic(2));
        b.push(ctx.rt(Rt::ObjNew));
        b.push(st(o));
        for (element, key) in [(0u32, names.stringify), (1u32, names.parse)] {
            box_object(&[ld(o)], b);
            b.push(ic(key));
            box_function(&[ld(element), ctx.rt(Rt::FnNew)], b);
            b.push(ctx.rt(Rt::ObjSet));
        }
        b.push(ld(o));
        f
    }
}

/// The set's names at this module's level, so that `emit` writes
/// `convert::build_json` beside `convert::build` rather than reaching through
/// a module whose only purpose is the attribute above. Unused for exactly as
/// long as that attribute is needed.
#[allow(unused_imports)]
pub(crate) use json::*;
