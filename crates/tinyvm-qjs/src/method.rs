//! **Research only.** Q1 of the method-binding track, variant C: call-site
//! specialisation. Deleted when Q1 is decided -- see
//! `plan/design-method-binding-experiment.md` and `research/method-binding/`.
//!
//! The variant's claim is that `a.map(f)` can be compiled to a direct call and
//! no function value need ever exist. This module holds the method *bodies*,
//! which are the part all three variants share: only the binding differs, so
//! only the binding is what the experiment compares. Keeping the bodies here
//! and shared is what makes "one separable implementation, packaged three
//! times" true rather than aspirational.
//!
//! # A finding, recorded at the moment it was found
//!
//! Call-site specialisation **cannot skip the run-time receiver test.** The
//! text says `x.trim()`; whether `x` is a String or an object with a `trim`
//! property is not decidable until it runs, and
//! `method_conformance::a_plain_object_property_named_like_a_method_is_untouched`
//! is the assertion that says so. So variant C saves the *function value*, not
//! the *dispatch* -- the branch just moves from the callee's value into the
//! call site's code. That belongs in criterion ⑥'s leak list.

use super::repr::{self, BlockType, Ins, ValType, WIDTH, box_string, unbox_string};
use super::runtime::{ALIGN_WORD, FnBuild, Rt, RtFunc};

/// Where this set sits, and where the unconditional runtime sits. The same
/// shape [`super::array::Ctx`] has, for the same reason: a gated set's own
/// index base is not the module's.
pub(crate) struct Ctx {
    /// Index of `__m_ws_width`.
    pub(crate) func_base: u32,
    /// Index of `__add` -- the base `runtime::SET` is laid out from.
    pub(crate) runtime_base: u32,
}

impl Ctx {
    fn me(&self, me: Me) -> Ins {
        Ins::Call(self.func_base + me.offset())
    }

    fn rt(&self, rt: Rt) -> Ins {
        Ins::Call(self.runtime_base + rt.offset())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Me {
    /// `ws_width(addr) -> i32`: the byte width of the WhiteSpace or
    /// LineTerminator character at `addr`, or 0 when it is neither. A helper
    /// rather than inline code because both ends of `trim` ask it.
    WsWidth,
    Trim,
}

pub(crate) const SET: &[Me] = &[Me::WsWidth, Me::Trim];

impl Me {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Me::WsWidth => "__m_ws_width",
            Me::Trim => "__m_trim",
        }
    }

    pub(crate) fn offset(self) -> u32 {
        SET.iter()
            .position(|m| *m == self)
            .expect("SET lists every Me") as u32
    }
}

pub(crate) fn build(ctx: &Ctx) -> Vec<RtFunc> {
    SET.iter().map(|m| one(ctx, *m)).collect()
}

fn values(n: usize) -> Vec<ValType> {
    (0..n).flat_map(|_| repr::SLOTS).collect()
}

fn one(ctx: &Ctx, me: Me) -> RtFunc {
    let i32_ = ValType::I32;
    let (params, results, f) = match me {
        Me::WsWidth => (vec![i32_], vec![i32_], ws_width()),
        Me::Trim => (values(1), values(1), trim(ctx)),
    };
    RtFunc {
        name: me.symbol(),
        params,
        results,
        locals: f.local_groups(),
        body: f.body,
    }
}

/// The width of the WhiteSpace or LineTerminator character at `addr`, or 0.
///
/// ECMA-262 22.1.3.32.1 (`TrimString`) trims WhiteSpace **and**
/// LineTerminator, which is 12.2's table plus 12.3's: TAB, LF, VT, FF, CR,
/// SP, NBSP, ZWNBSP, LS, PS, and the Unicode `Zs` category.
///
/// **What is here and what is not.** The named ten are here. The rest of `Zs`
/// -- U+1680, U+2000..U+200A, U+202F, U+205F, U+3000 -- is **not**, and that
/// is a known gap rather than an oversight: covering it means a range table,
/// and this is a research variant whose job is to price a *binding mechanism*.
/// The gap costs every variant the same number of bytes, so it cannot move the
/// comparison; it is recorded in `research/method-binding/README.md` so it
/// cannot be forgotten if this body is ever promoted.
fn ws_width() -> FnBuild {
    let mut f = FnBuild::new(1);
    let b0 = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 0));
    b.push(Ins::LocalSet(b0));

    // The six one-byte ones: TAB(09) LF(0a) VT(0b) FF(0c) CR(0d) and SP(20).
    // `b0 - 9 <= 4` catches 09..0d in one comparison.
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(9));
    b.push(Ins::I32Sub);
    // `< 5` rather than `<= 4`: the IR has no `i32.le_u`, and adding one to
    // production code for a research variant is exactly the kind of quiet
    // widening this experiment is supposed to detect rather than commit.
    b.push(Ins::I32Const(5));
    b.push(Ins::I32LtU);
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0x20));
    b.push(Ins::I32Eq);
    b.push(Ins::I32Or);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(1));
    b.push(Ins::Return);
    b.push(Ins::End);

    // NBSP U+00A0 is C2 A0.
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xc2));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0xa0));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(2));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::End);

    // The three-byte ones: LS U+2028 (E2 80 A8), PS U+2029 (E2 80 A9),
    // ZWNBSP U+FEFF (EF BB BF).
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xe2));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0xa8));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(2));
    b.push(Ins::I32LtU);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(3));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xef));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0xbb));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0xbf));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(3));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::I32Const(0));
    f
}

/// `"  ab  ".trim()` -- ECMA-262 22.1.3.32.
///
/// Traps on a non-String receiver, from `unbox_string`. The receiver *test*
/// is the call site's job in this variant; by the time control is here the
/// answer is already meant to be a String.
fn trim(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let p = f.local(ValType::I32);
    let len = f.local(ValType::I32);
    let start = f.local(ValType::I32);
    let end = f.local(ValType::I32);
    let q = f.local(ValType::I32);
    let w = f.local(ValType::I32);
    let out = f.local(ValType::I32);
    let i = f.local(ValType::I32);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(p));

    let b = &mut f.body;
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(len));
    b.push(Ins::LocalGet(len));
    b.push(Ins::LocalSet(end));

    // Forward: skip whole whitespace characters.
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(start));
    b.push(Ins::LocalGet(len));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(start));
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.me(Me::WsWidth));
    b.push(Ins::LocalSet(w));
    b.push(Ins::LocalGet(w));
    b.push(Ins::I32Eqz);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(start));
    b.push(Ins::LocalGet(w));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(start));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    // Backward: step to the start of the character before `end` -- back over
    // continuation bytes -- and stop at the first that is not whitespace.
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(start));
    b.push(Ins::LocalGet(end));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(end));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalSet(q));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(start));
    b.push(Ins::LocalGet(q));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(q));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Const(0xc0));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Ne);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(q));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalSet(q));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(q));
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.me(Me::WsWidth));
    b.push(Ins::I32Eqz);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(q));
    b.push(Ins::LocalSet(end));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    // Allocate `end - start` bytes and copy.
    b.push(Ins::LocalGet(end));
    b.push(Ins::LocalGet(start));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(out));
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(end));
    b.push(Ins::LocalGet(start));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Store(ALIGN_WORD, 0));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(end));
    b.push(Ins::LocalGet(start));
    b.push(Ins::I32Sub);
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(p));
    b.push(Ins::LocalGet(start));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Store8(0, 4));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    let mut inner = Vec::new();
    inner.push(Ins::LocalGet(out));
    let mut tail = Vec::new();
    box_string(&inner, &mut tail);
    f.body.extend(tail);
    f
}
