//! Methods on built-in receivers: `trim`, `indexOf`, `push`, `pop`, `map`.
//!
//! # Why the binding works the way it does
//!
//! A method call needs the receiver to reach the body, and there are three
//! ways to arrange that. Which one this engine uses was **measured**, not
//! argued: `research/method-binding/` implemented all three -- pass it through
//! a `this` in the calling convention, capture it in a closure at the property
//! read, or specialise at the call site -- ran every one against the same
//! variant-independent corpus, and compared them on marginal cost. The call
//! site won. `RESULTS.md` there has the decision trace and the numbers.
//!
//! The consequence worth knowing here: **the run-time receiver test cannot be
//! skipped.** The text says `x.trim()`, and whether `x` is a String or an
//! object carrying a `trim` property is not decidable until it runs --
//! `method_conformance::a_plain_object_property_named_like_a_method_is_untouched`
//! is the assertion that says the second must keep working. So what the call
//! site removes is the *function value*, not the *dispatch*. That costs about
//! 43-53 bytes per call site, and it is what the losing designs traded away
//! by charging every unrelated function value instead.

use super::repr::{
    self, BlockType, Ins, ValType, WIDTH, box_number, box_string, unbox_array, unbox_string,
};
// `map`'s prefab is the one function that builds an array and calls back into
// a function value; `box_function` is variant A's and B's property read.
use super::repr::{box_array, const_undefined};
use super::array::{ARR_ELEMS, ARR_LEN, Ar, ELEM_BYTES, ELEM_PAYLOAD, ELEM_TAG};
use super::runtime::{ALIGN_WORD, FN_ELEMENT, FN_ENV, FnBuild, Rt, RtFunc};

/// Where this set sits, and where the unconditional runtime sits. The same
/// shape [`super::array::Ctx`] has, for the same reason: a gated set's own
/// index base is not the module's.
pub(crate) struct Ctx {
    /// Index of this set's first function.
    pub(crate) func_base: u32,
    /// Index of `__add` -- the base `runtime::SET` is laid out from.
    pub(crate) runtime_base: u32,
    /// Which functions this module carries, and where.
    pub(crate) plan: Plan,
    /// The uniform call signature's type index, and the arity it pads to.
    ///
    /// Nothing in this compiler's runtime could `call_indirect` before
    /// `__m_map_bound`, which has to: its argument is a function value. The
    /// experiment assumed that was impossible and designed around it; finding
    /// out it was not is what let `map` become an ordinary prefab call, and
    /// cut its cost per call site by more than three times.
    pub(crate) uniform: Option<(u32, u32)>,
    /// Index of `__arr_new`, the base [`super::array::SET`] is laid out from.
    ///
    /// **A cross-gate dependency, and a leak.** `__m_push` cannot append
    /// without `__arr_push`, which lives in the *array* set behind the *array*
    /// gate. So a call site wanting `push` has to reach across and turn a
    /// second, unrelated gate on. Variants A and B would have the same
    /// dependency -- the method body is shared -- but they would not need the
    /// *emitter* to know about it, because the body would be reached through a
    /// value rather than through an index this module has to compute.
    pub(crate) array_base: u32,
}

impl Ctx {
    fn me(&self, me: Me) -> Ins {
        Ins::Call(self.func_base + self.plan.offset(me))
    }

    fn rt(&self, rt: Rt) -> Ins {
        Ins::Call(self.runtime_base + rt.offset())
    }

    fn arr(&self, ar: Ar) -> Ins {
        Ins::Call(self.array_base + ar.offset())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Me {
    /// `ws_width(addr) -> i32`: the byte width of the WhiteSpace or
    /// LineTerminator character at `addr`, or 0 when it is neither. A helper
    /// rather than inline code because both ends of `trim` ask it.
    WsWidth,
    /// `units(p, n) -> i32`: UTF-16 code units in the first `n` bytes of the
    /// string body at `p`. `indexOf` reports a position, and a position has
    /// to be in the same unit `length` counts in or the two disagree.
    Units,
    Trim,
    IndexOf,
    Push,
    /// `a.pop()` -- ECMA-262 23.1.3.22. The **fifth** method, added only to
    /// measure criterion ④: how many variant-specific lines a new method
    /// costs. The body below is shared; what each variant adds is counted in
    /// `research/method-binding/RESULTS.md`.
    Pop,
    /// `a.map(f)` as a **prefab**, which leak 4 said could not exist: it has
    /// to `call_indirect` into the callback. It can, once the prefab layer is
    /// handed the uniform type index.
    ///
    /// **All three variants use it.** That is the correction: variant C used
    /// to inline the loop at every call site, and only because a prefab was
    /// believed unable to call back. Once B disproved that, C's `map` became
    /// an ordinary prefab call like its other three, and the inlined form was
    /// deleted. Re-measuring after that fix is what §2.6 of the skill demands
    /// before judging a variant -- and it changed C's number by 4x.
    MapBound,
}

/// Every function this variant can emit, in module order. **Not** what a
/// given module emits: see [`Plan`].
pub(crate) const SET: &[Me] = &[
    Me::WsWidth,
    Me::Units,
    Me::Trim,
    Me::IndexOf,
    Me::Push,
    Me::Pop,
    Me::MapBound,
];


/// Which of [`SET`] a particular module carries, and where each one lands.
///
/// Whole-set gating made a program that calls only `trim()` pay 307 bytes for
/// `indexOf`, which is criterion ③ growing linearly in the number of methods
/// -- the shape §4's decision tree judges negative. This is the fix, and the
/// fix is itself a cost chargeable to variant C: A and B need nothing like it,
/// because their methods are function values and only a referenced one has to
/// exist.
///
/// The awkward part, and the reason it is a `Plan` rather than a filter: a
/// function's index is its **position**, so dropping one moves every later
/// one. Offsets are therefore computed against the enabled subset instead of
/// against `SET` -- the same problem `Rt::Len` had in
/// `plan/design-string-length-milestone.md` §3, met a second time.
#[derive(Debug, Default, Clone)]
pub(crate) struct Plan {
    enabled: Vec<Me>,
}

impl Plan {
    /// Add a method and everything it needs. `trim` pulls in `ws_width`,
    /// `indexOf` pulls in `units`; a helper nobody needs is not emitted, which
    /// is the whole point.
    pub(crate) fn want(&mut self, me: Me) {
        for needed in [me].into_iter().chain(me.helpers()) {
            // A method with no prefab -- `map` -- asks for nothing here. It is
            // emitted at the call site, so there is no function to place.
            if SET.contains(&needed) && !self.enabled.contains(&needed) {
                self.enabled.push(needed);
            }
        }
    }


    pub(crate) fn is_empty(&self) -> bool {
        self.enabled.is_empty()
    }

    pub(crate) fn len(&self) -> u32 {
        self.enabled.len() as u32
    }

    /// In `SET` order, so the module's layout does not depend on the order
    /// call sites happened to appear in the source.
    fn ordered(&self) -> Vec<Me> {
        SET.iter().copied().filter(|m| self.enabled.contains(m)).collect()
    }

    pub(crate) fn offset(&self, me: Me) -> u32 {
        self.ordered()
            .iter()
            .position(|m| *m == me)
            .expect("a call site asked for a method the plan does not carry") as u32
    }
}

impl Me {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Me::WsWidth => "__m_ws_width",
            Me::Units => "__m_units",
            Me::Trim => "__m_trim",
            Me::IndexOf => "__m_index_of",
            Me::Push => "__m_push",
            Me::Pop => "__m_pop",
            Me::MapBound => "__m_map_bound",
        }
    }

    /// The method a `recv.name(args)` call site denotes, or `None` when this
    /// variant does not specialise it. The arity is part of the question: a
    /// `trim` called with an argument is not this `trim`, and specialising it
    /// would be inventing a signature.
    pub(crate) fn at_call_site(name: &str, argc: usize) -> Option<Self> {
        match (name, argc) {
            ("trim", 0) => Some(Me::Trim),
            ("indexOf", 1) => Some(Me::IndexOf),
            ("push", 1) => Some(Me::Push),
            ("pop", 0) => Some(Me::Pop),
            ("map", 1) => Some(Me::MapBound),
            _ => None,
        }
    }

    /// The tag a call site must see before it may take the fast path. Two
    /// methods, two receivers -- so the call site's type test is per method,
    /// not one shared test. Small, but it is the call site that carries it,
    /// which is the shape criterion ⑥ is collecting.
    pub(crate) fn receiver_is_array(self) -> bool {
        matches!(self, Me::Push | Me::Pop | Me::MapBound)
    }

    /// Whether this method's body reaches into the array set, which the
    /// *array* gate controls. See [`Ctx::array_base`].
    pub(crate) fn needs_arrays(self) -> bool {
        matches!(self, Me::Push | Me::MapBound)
    }



    /// What this method's body calls, so [`Plan`] can pull them in.
    fn helpers(self) -> Vec<Me> {
        match self {
            Me::Trim => vec![Me::WsWidth],
            Me::IndexOf => vec![Me::Units],
            Me::WsWidth | Me::Units | Me::Push | Me::Pop | Me::MapBound => Vec::new(),
        }
    }
}

pub(crate) fn build(ctx: &Ctx) -> Vec<RtFunc> {
    ctx.plan.ordered().iter().map(|m| one(ctx, *m)).collect()
}

fn values(n: usize) -> Vec<ValType> {
    (0..n).flat_map(|_| repr::SLOTS).collect()
}

fn one(ctx: &Ctx, me: Me) -> RtFunc {
    let i32_ = ValType::I32;
    let (params, results, f) = match me {
        Me::WsWidth => (vec![i32_], vec![i32_], ws_width()),
        Me::Units => (vec![i32_, i32_], vec![i32_], units()),
        Me::Trim => (values(1), values(1), trim(ctx)),
        Me::IndexOf => (values(2), values(1), index_of(ctx)),
        Me::Push => (values(2), values(1), push(ctx)),
        Me::Pop => (values(1), values(1), pop()),
        Me::MapBound => (values(2), values(1), map_bound(ctx)),
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
/// **The whole set is here**, including the `Zs` members beyond the named ten
/// (U+1680, U+2000..U+200A, U+202F, U+205F, U+3000). They were left out while
/// this was research code -- the gap cost every variant the same, so it could
/// not move the comparison -- and closing it was the precondition recorded for
/// promoting this body, because the alternative is a **wrong answer**:
/// `"\u{2003}a".trim()` would keep the space and nothing would say so.
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
    // production code for one method would be a quiet widening of the IR for
    // a caller's convenience.
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

    // The three-byte ones. `E2 80 xx` covers U+2000..U+200A (the bulk of
    // `Zs`), LS U+2028, PS U+2029, and NNBSP U+202F; `E2 81 9F` is MMSP
    // U+205F; `E1 9A 80` is OGHAM SPACE MARK U+1680; `E3 80 80` is
    // IDEOGRAPHIC SPACE U+3000; `EF BB BF` is ZWNBSP U+FEFF.
    //
    // These are the rest of the `Zs` category, and they are here rather than
    // deferred because the alternative is a **wrong answer**: `"\u{2003}a".trim()`
    // would keep the space and no diagnostic would say so. ECMA-262 12.2's
    // WhiteSpace is `Zs` plus TAB/VT/FF/ZWNBSP, and 22.1.3.32.1 trims that
    // set plus 12.3's LineTerminators. This is that set, exactly.
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xe2));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    // U+2000..U+200A
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(11));
    b.push(Ins::I32LtU);
    // U+2028, U+2029
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0xa8));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(2));
    b.push(Ins::I32LtU);
    b.push(Ins::I32Or);
    // U+202F
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0xaf));
    b.push(Ins::I32Eq);
    b.push(Ins::I32Or);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(3));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::End);
    // U+205F
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0x81));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0x9f));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(3));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::End);

    // U+1680 (E1 9A 80) and U+3000 (E3 80 80).
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xe1));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0x9a));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(3));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xe3));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(3));
    b.push(Ins::Return);
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

/// UTF-16 code units in the first `n` bytes of the string body at `p`.
///
/// The same rule `runtime::length` uses -- a non-continuation byte starts a
/// character, and a byte at or above `0xf0` is a surrogate pair -- applied to
/// a prefix rather than to the whole. Duplicated rather than shared because
/// `__len` takes a JS value and counts all of it, and widening its signature
/// for one caller would be the wrong trade.
fn units() -> FnBuild {
    let mut f = FnBuild::new(2);
    let i = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let byte = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::LocalSet(byte));
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
    b.push(Ins::LocalGet(n));
    f
}

/// `"abc".indexOf("b")` -- ECMA-262 22.1.3.9, without the second argument.
///
/// **A byte search is sound here.** The needle is valid UTF-8, so its first
/// byte is a lead byte, and a lead byte never appears as a continuation byte
/// in valid UTF-8. A byte-offset match can therefore only land on a character
/// boundary, and no separate boundary check is needed. The offset is then
/// converted to code units, because a position that did not agree with
/// `length` would be a position no script could use.
fn index_of(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let h = f.local(ValType::I32);
    let nd = f.local(ValType::I32);
    let hl = f.local(ValType::I32);
    let nl = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let j = f.local(ValType::I32);
    let ok = f.local(ValType::I32);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(h));
    unbox_string(WIDTH, &mut f.body);
    f.body.push(Ins::LocalSet(nd));

    let b = &mut f.body;
    b.push(Ins::LocalGet(h));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(hl));
    b.push(Ins::LocalGet(nd));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(nl));

    // 22.1.3.9 step 6: an empty needle is found at 0, whatever the haystack.
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    number_const(b, 0);
    b.push(Ins::Return);
    b.push(Ins::End);

    // A needle longer than the haystack cannot be found -- and testing it
    // first is what keeps `hl - nl` below from wrapping.
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    number_const(b, -1);
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32LtU);
    b.push(Ins::BrIf(1));

    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(ok));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(j));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(j));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::LocalGet(nd));
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(ok));
    b.push(Ins::End);
    b.push(Ins::LocalGet(ok));
    b.push(Ins::I32Eqz);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(j));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(ok));
    b.push(Ins::If(BlockType::Empty));
    let mut found = Vec::new();
    found.push(Ins::LocalGet(h));
    found.push(Ins::LocalGet(i));
    found.push(ctx.me(Me::Units));
    found.push(Ins::F64ConvertI32S);
    let mut boxed = Vec::new();
    box_number(&found, &mut boxed);
    b.extend(boxed);
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    number_const(b, -1);
    f
}



/// Push a Number literal as a V1 pair.
fn number_const(b: &mut Vec<Ins>, v: i32) {
    let mut inner = Vec::new();
    inner.push(Ins::F64Const(v as f64));
    let mut out = Vec::new();
    box_number(&inner, &mut out);
    b.extend(out);
}

/// `[1, 2].push(3)` -- ECMA-262 23.1.3.23, without the rest parameter.
///
/// Traps on a non-Array receiver, from `unbox_array`. The append itself is
/// `__arr_push`, which already exists and already grows the vector; this adds
/// only the unboxing and the return value, which 23.1.3.23 step 5 makes the
/// **new length**.
fn push(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let a = f.local(ValType::I32);

    unbox_array(0, &mut f.body);
    f.body.push(Ins::LocalSet(a));

    let b = &mut f.body;
    b.push(Ins::LocalGet(a));
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::LocalGet(WIDTH + 1));
    b.push(ctx.arr(Ar::Push));

    let mut inner = Vec::new();
    inner.push(Ins::LocalGet(a));
    inner.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    inner.push(Ins::F64ConvertI32S);
    let mut out = Vec::new();
    box_number(&inner, &mut out);
    f.body.extend(out);
    f
}

/// `a.map(f)` -- ECMA-262 23.1.3.20.
///
/// The receiver and the callback, both as V1 pairs. This is the one prefab
/// that calls **back** into a guest function value, which is why the set needs
/// the uniform signature's type index at all.
fn map_bound(ctx: &Ctx) -> FnBuild {
    let (type_index, arity) = ctx
        .uniform
        .expect("variant B's map needs the uniform signature");
    // Under A the receiver arrives as a pair rather than as an environment
    // pointer, so the parameter list is one slot wider.
    let mut f = FnBuild::new(2 * WIDTH);
    let a = f.local(ValType::I32);
    let out = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let tag = f.local(ValType::I32);
    let payload = f.local(ValType::I64);

    f.body.push(Ins::LocalGet(0));
    f.body.push(Ins::LocalGet(1));
    f.body.push(Ins::LocalSet(payload));
    f.body.push(Ins::LocalSet(tag));
    unbox_array_from(tag, payload, &mut f.body);
    f.body.push(Ins::LocalSet(a));

    let b = &mut f.body;
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalSet(n));
    b.push(Ins::LocalGet(n));
    b.push(ctx.arr(Ar::New));
    b.push(Ins::LocalSet(out));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));

    // The callback's own environment goes first: the uniform signature leads
    // with it, and the callback's record is what holds the answer.
    let cb = 3;
    b.push(Ins::LocalGet(cb));
    b.push(Ins::I32WrapI64);
    b.push(Ins::I32Load(ALIGN_WORD, FN_ENV));

    b.push(Ins::LocalGet(a));
    b.push(Ins::LocalGet(i));
    b.push(ctx.arr(Ar::Get));
    for _ in 1..arity {
        const_undefined(b);
    }
    b.push(Ins::LocalGet(cb));
    b.push(Ins::I32WrapI64);
    b.push(Ins::I32Load(ALIGN_WORD, FN_ELEMENT));
    b.push(Ins::CallIndirect(type_index, 0));
    b.push(Ins::LocalSet(payload));
    b.push(Ins::LocalSet(tag));

    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(tag));
    b.push(Ins::LocalGet(payload));
    b.push(ctx.arr(Ar::Push));

    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    let mut inner = Vec::new();
    inner.push(Ins::LocalGet(out));
    let mut boxed = Vec::new();
    box_array(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `unbox_array`, but from two locals rather than from a parameter pair.
fn unbox_array_from(tag: u32, payload: u32, out: &mut Vec<Ins>) {
    out.push(Ins::LocalGet(tag));
    out.push(Ins::I32Const(repr::TAG_ARRAY));
    out.push(Ins::I32Ne);
    out.push(Ins::If(BlockType::Empty));
    out.push(Ins::Unreachable);
    out.push(Ins::End);
    out.push(Ins::LocalGet(payload));
    out.push(Ins::I32WrapI64);
}

/// `[1, 2].pop()` -- ECMA-262 23.1.3.22. The fifth method, for criterion ④.
///
/// Returns the last element and shortens the array; `undefined` and no change
/// when it is empty (step 3a).
fn pop() -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let a = f.local(ValType::I32);
    let n = f.local(ValType::I32);

    unbox_array(0, &mut f.body);
    f.body.push(Ins::LocalSet(a));

    let b = &mut f.body;
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalSet(n));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    const_undefined(b);
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(a));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalTee(n));
    b.push(Ins::I32Store(ALIGN_WORD, ARR_LEN));

    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(a));
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::LocalGet(a));
    b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
    f
}
