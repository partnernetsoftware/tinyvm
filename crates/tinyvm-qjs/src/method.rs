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

use super::repr::{
    self, BlockType, Ins, ValType, WIDTH, box_number, box_string, unbox_array, unbox_string,
};
// `map`'s prefab is the one function that builds an array and calls back into
// a function value; `box_function` is variant A's and B's property read.
use super::repr::{box_array, const_undefined};
#[cfg(any(feature = "method-bound", feature = "method-this"))]
use super::repr::box_function;
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
    /// Research only -- Q1 variant B. The uniform call signature's type
    /// index, and the arity it pads to.
    ///
    /// **This is the capability leak 4 said was missing**, handed to the
    /// prefab layer. Nothing in this compiler's runtime could `call_indirect`
    /// before; `__m_map_bound` has to, because its argument is a function
    /// value. Variant C never needs it -- it inlines the loop where the
    /// instruction already is -- so the whole of this field, and the plumbing
    /// that fills it, is chargeable to B.
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
    /// Research only -- Q1 variants A and B. See [`bind`]: the property read
    /// hands back a function value. Under B its environment holds the
    /// receiver; under A the record is plain and the receiver arrives at call
    /// time instead. One variant of the enum, two meanings, and that
    /// difference is the whole of what Q1 compares.
    #[cfg(any(feature = "method-bound", feature = "method-this"))]
    Bind,
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
    #[cfg(any(feature = "method-bound", feature = "method-this"))]
    Me::Bind,
    Me::MapBound,
];

/// Research only -- Q1 variant B. `Me::Bind`'s helper set is empty, so a
/// surface built from [`Me::BOUND`] needs `want` to pull helpers in.
#[cfg(any(feature = "method-bound", feature = "method-this"))]
pub(crate) const _BOUND_SURFACE_USES_WANT: () = ();

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

    /// Variant B's plan, as far as B is implemented: `trim` and what it
    /// needs, plus the binder.
    ///
    /// **Not per *call site*, unlike variant C's.** B cannot gate on whether
    /// the source names `trim`, because the element index is baked into
    /// `obj_get`, which is emitted before anyone knows. So any program that
    /// can read a String property carries the whole of B's exposed surface.
    /// C pays nothing for a method it does not call; B pays for every method
    /// it exposes. That asymmetry is a result, not an implementation detail.
    #[cfg(any(feature = "method-bound", feature = "method-this"))]
    pub(crate) fn bound_surface(bound: &[Me]) -> Self {
        let mut plan = Self {
            enabled: vec![Me::Bind],
        };
        for me in bound {
            plan.want(*me);
        }
        plan
    }

    #[cfg_attr(
        any(feature = "method-bound", feature = "method-this"),
        allow(dead_code, reason = "variant C asks; variant B's surface is never empty")
    )]
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
            #[cfg(any(feature = "method-bound", feature = "method-this"))]
            Me::Bind => "__m_bind",
            Me::MapBound => "__m_map_bound",
        }
    }

    #[cfg_attr(
        any(feature = "method-bound", feature = "method-this"),
        allow(dead_code, reason = "variant C's call-site machinery")
    )]
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

    #[cfg_attr(
        any(feature = "method-bound", feature = "method-this"),
        allow(dead_code, reason = "variant C's call-site machinery")
    )]
    /// The tag a call site must see before it may take the fast path. Two
    /// methods, two receivers -- so the call site's type test is per method,
    /// not one shared test. Small, but it is the call site that carries it,
    /// which is the shape criterion ⑥ is collecting.
    pub(crate) fn receiver_is_array(self) -> bool {
        matches!(self, Me::Push | Me::Pop | Me::MapBound)
    }

    #[cfg_attr(
        any(feature = "method-bound", feature = "method-this"),
        allow(dead_code, reason = "variant C's call-site machinery")
    )]
    /// Whether this method's body reaches into the array set, which the
    /// *array* gate controls. See [`Ctx::array_base`].
    pub(crate) fn needs_arrays(self) -> bool {
        matches!(self, Me::Push | Me::MapBound)
    }

    /// Where this function sits, given the runtime `Ctx` that knows the plan.
    /// A shim so `runtime.rs` does not have to carry a `Plan`.
    /// Where this function sits in variant B's surface, given which methods
    /// the program reads by name.
    #[cfg(any(feature = "method-bound", feature = "method-this"))]
    pub(crate) fn offset_in_bound(self, bound: &[Me]) -> u32 {
        Plan::bound_surface(bound).offset(self)
    }

    /// Research only -- Q1 variant B. The bound methods, in the fixed order
    /// their table elements are reserved in: the source name, the body to
    /// call, how many arguments the adapter forwards, and whether the
    /// receiver is an Array (so the arm goes in `array::prop_get`) or a
    /// String (so it goes in `runtime::obj_get`).
    #[cfg(any(feature = "method-bound", feature = "method-this"))]
    pub(crate) const BOUND: &'static [(&'static str, Me, u32, bool)] = &[
        ("trim", Me::Trim, 0, false),
        ("indexOf", Me::IndexOf, 1, false),
        ("push", Me::Push, 1, true),
        ("pop", Me::Pop, 0, true),
        // `map`'s body reads the receiver out of the environment itself,
        // because it also has to `call_indirect`; its adapter forwards the
        // environment rather than unpacking it.
        ("map", Me::MapBound, 1, true),
    ];

    /// What this method's body calls, so [`Plan`] can pull them in.
    fn helpers(self) -> Vec<Me> {
        match self {
            Me::Trim => vec![Me::WsWidth],
            Me::IndexOf => vec![Me::Units],
            #[cfg(any(feature = "method-bound", feature = "method-this"))]
            Me::Bind => Vec::new(),
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
    let i64_ = ValType::I64;
    let (params, results, f) = match me {
        Me::WsWidth => (vec![i32_], vec![i32_], ws_width()),
        Me::Units => (vec![i32_, i32_], vec![i32_], units()),
        Me::Trim => (values(1), values(1), trim(ctx)),
        Me::IndexOf => (values(2), values(1), index_of(ctx)),
        Me::Push => (values(2), values(1), push(ctx)),
        Me::Pop => (values(1), values(1), pop()),
        // Unreachable: `Plan::want` never places a method that has no prefab,
        // and `build` only walks the plan.
        #[cfg(any(feature = "method-bound", feature = "method-this"))]
        Me::Bind => (vec![i32_, i64_, i32_], values(1), bind(ctx)),
        Me::MapBound => (
            if cfg!(feature = "method-bound") {
                vec![i32_, i32_, i64_]
            } else {
                values(2)
            },
            values(1),
            map_bound(ctx),
        ),
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
/// `__len` takes a JS value and counts all of it; a research variant does not
/// get to widen a production signature.
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

/// Research only -- Q1 variant B. The environment a bound method carries.
///
/// Twelve bytes: the receiver, stored as the V1 pair it already is. **A second
/// environment layout**, and that is a leak: the closure machinery's
/// environment is a vector of *cells*, because a closure's binding can be
/// written through. A bound receiver cannot -- 23.1.3's methods do not rebind
/// their `this` -- so a cell would be a word of indirection per call for a
/// mutation that cannot happen. Cheaper, and one more shape in the engine.
#[cfg(any(feature = "method-bound", feature = "method-this"))]
pub(crate) const BOUND_BYTES: i32 = 12;

/// `__m_bind(recv_tag, recv_payload, element) -> (tag, payload)`, variant B's
/// whole mechanism: a property read hands back a function value whose
/// environment already holds the receiver.
///
/// Where variant C puts a type test at every call site, this puts an
/// allocation at every property *read*. Which is cheaper is exactly what
/// criterion ③b is for.
#[cfg(any(feature = "method-bound", feature = "method-this"))]
pub(crate) fn bind(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH + 1);
    let env = f.local(ValType::I32);
    let b = &mut f.body;

    // Research only -- Q1 variant A. The receiver does **not** travel in the
    // value: it arrives at call time through the calling convention. So the
    // property read allocates a plain function record and the two parameters
    // holding the receiver are simply unread.
    //
    // That is the one line of difference between A and B, and it is why they
    // share everything else: same table element, same adapter shape, same
    // bodies. What differs is *when* the receiver is captured -- at the read
    // (B) or at the call (A) -- which is exactly the question Q1 asks.
    if cfg!(feature = "method-this") {
        let mut inner = Vec::new();
        inner.push(Ins::LocalGet(WIDTH));
        inner.push(Ins::I32Const(0));
        inner.push(ctx.rt(Rt::FnNew));
        let mut out = Vec::new();
        box_function(&inner, &mut out);
        f.body.extend(out);
        return f;
    }

    b.push(Ins::I32Const(BOUND_BYTES));
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalTee(env));
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Store(ALIGN_WORD, 0));
    b.push(Ins::LocalGet(env));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I64Store(ALIGN_WORD, 4));

    let mut inner = Vec::new();
    inner.push(Ins::LocalGet(WIDTH));
    inner.push(Ins::LocalGet(env));
    inner.push(ctx.rt(Rt::FnNew));
    let mut out = Vec::new();
    box_function(&inner, &mut out);
    f.body.extend(out);
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

/// Research only -- Q1 variant B. `a.map(f)` as a prefab.
///
/// Parameters are the environment (holding the receiver), then the callback as
/// a V1 pair -- the shape the adapter forwards. The loop is the same one
/// variant C inlines at every call site; here it exists **once**, which is
/// exactly the trade the two variants are being measured on.
#[cfg(any(
    feature = "method-bound",
    feature = "method-this",
    feature = "method-callsite"
))]
fn map_bound(ctx: &Ctx) -> FnBuild {
    let (type_index, arity) = ctx
        .uniform
        .expect("variant B's map needs the uniform signature");
    // Under A the receiver arrives as a pair rather than as an environment
    // pointer, so the parameter list is one slot wider.
    // The receiver is a pair everywhere except variant B, where it comes out
    // of the environment the property read allocated.
    let mut f = FnBuild::new(if cfg!(feature = "method-bound") {
        1 + WIDTH
    } else {
        2 * WIDTH
    });
    let a = f.local(ValType::I32);
    let out = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let tag = f.local(ValType::I32);
    let payload = f.local(ValType::I64);

    // The receiver: out of the environment under B, straight off the
    // parameters under A. Same body either way, which is the point.
    if cfg!(feature = "method-bound") {
        f.body.push(Ins::LocalGet(0));
        f.body.push(Ins::I32Load(ALIGN_WORD, 0));
        f.body.push(Ins::LocalGet(0));
        f.body.push(Ins::I64Load(ALIGN_WORD, 4));
    } else {
        f.body.push(Ins::LocalGet(0));
        f.body.push(Ins::LocalGet(1));
    }
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
    let cb = if cfg!(feature = "method-bound") { 2 } else { 3 };
    b.push(Ins::LocalGet(cb));
    b.push(Ins::I32WrapI64);
    b.push(Ins::I32Load(ALIGN_WORD, FN_ENV));

    // Under A the uniform signature carries a receiver, and 23.1.3.20 calls
    // the callback with `undefined` as its `this` when no thisArg is given.
    if cfg!(feature = "method-this") {
        const_undefined(b);
    }
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
