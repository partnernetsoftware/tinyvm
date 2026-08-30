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
    self, BlockType, Ins, ValType, WIDTH, box_number, box_string, unbox_array, unbox_number,
    unbox_string,
};
// `map`'s prefab is the one function that builds an array and calls back into
// a function value; `box_function` is variant A's and B's property read.
use super::array::{ARR_ELEMS, ARR_LEN, Ar, ELEM_BYTES, ELEM_PAYLOAD, ELEM_TAG};
use super::repr::{box_array, const_bool, const_undefined, unbox_object};
use super::runtime::{
    ALIGN_WORD, ENTRY_BYTES, ENTRY_KEY, FAULT_CAPABILITY, FN_ELEMENT, FN_ENV, FnBuild, OBJ_ENTRIES,
    OBJ_LEN, RefusalNames, Rt, RtFunc, record_named_fault,
};

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
    /// The boundaries `split("")` and a mid-surrogate `slice` name; `None`
    /// when the program wants neither method.
    pub(crate) refusal_names: Option<RefusalNames>,
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
    /// Guest address of the lowercase run table, and how many runs it holds.
    ///
    /// Placed in the literal pool by the emitter only when the plan wants
    /// [`Me::LowerCp`], so a program that never lowercases carries neither the
    /// table nor the search. Zero when absent, which is safe because nothing
    /// reads it then.
    pub(crate) case_table: i32,
    pub(crate) case_runs: u32,
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
    /// `s.includes(t)` -- ECMA-262 22.1.3.8. **The most-demanded method in the
    /// corpus**: the second survey counts `.contains(` in 58 of 82 downstream
    /// scripts, 721 times, ahead of everything else measured.
    ///
    /// Not `indexOf(t) !== -1`, though it answers the same question. `indexOf`
    /// reports a *position*, so it drags in [`Me::Units`] to convert a byte
    /// offset into UTF-16 code units; a Boolean needs no position and
    /// therefore no helper. The search loop is duplicated rather than shared
    /// for that reason -- sharing it would make the cheapest method pay for
    /// the more expensive one's arithmetic.
    Includes,
    /// `s.startsWith(t)` -- ECMA-262 22.1.3.23.
    StartsWith,
    /// `s.endsWith(t)` -- ECMA-262 22.1.3.7.
    EndsWith,
    /// `substr(p, start, n) -> i32`: a fresh string of `n` bytes from `p +
    /// start`. A helper, like [`Me::WsWidth`] and [`Me::Units`].
    Substr,
    /// `s.split(sep)` -- ECMA-262 22.1.3.23, third in the second survey at 34
    /// of 82 scripts and 129 uses.
    Split,
    /// `decode(p) -> (cp, width)`: one UTF-8 character.
    Decode,
    /// `encode(dst, cp) -> width`: one UTF-8 character.
    Encode,
    /// `lower_cp(cp) -> cp`: the Unicode simple lowercase mapping.
    LowerCp,
    /// `s.replace(from, to)` -- ECMA-262 22.1.3.19, first occurrence only.
    Replace,
    /// `s.replaceAll(from, to)` -- ECMA-262 22.1.3.20. What the corpus means
    /// when it writes `.replace`, which is why both ship.
    ReplaceAll,
    /// `Object.keys(o)` -- ECMA-262 20.1.2.17, arriving as `o.__keys()` from
    /// the parser's fold. Every entry's key, in insertion order, as a new
    /// array of Strings.
    ObjKeys,
    /// `s.toLowerCase()` -- ECMA-262 22.1.3.29. Fourth in the second survey,
    /// and the one whose price was measured before it was chosen: see
    /// `plan/design-case-mapping-decision.md`.
    ToLowerCase,
    /// `s.slice(start, end)` -- ECMA-262 22.1.3.21, both indices given.
    /// Positions are UTF-16 code units, as `length` and `indexOf` count
    /// them; the byte work is in [`Me::SliceCore`], shared with the
    /// one-argument form so a program using both pays for one body.
    Slice,
    /// `s.slice(start)`: `end` defaults to the length. Its own variant
    /// because a call site's arity is part of what it denotes (see
    /// [`Me::at_call_site`]); the body is three instructions around the core.
    SliceFrom,
    /// `(record, from_units, to_units) -> record`, the shared body of the two
    /// `slice` forms: code-unit positions to byte offsets in one pass, then
    /// [`Me::Substr`]. A boundary that falls inside a surrogate pair is a
    /// lone surrogate, which UTF-8 cannot carry: that traps, for the reason
    /// `split("")` gives.
    SliceCore,
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
    Me::Includes,
    Me::StartsWith,
    Me::EndsWith,
    Me::Substr,
    Me::Split,
    Me::Decode,
    Me::Encode,
    Me::LowerCp,
    Me::ToLowerCase,
    Me::Slice,
    Me::SliceFrom,
    Me::SliceCore,
    Me::Replace,
    Me::ReplaceAll,
    Me::ObjKeys,
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

    /// Whether this plan carries a particular member.
    ///
    /// Asked by the emitter for exactly one thing: whether to place the
    /// lowercase table. A `Plan` is otherwise consulted only for offsets, and
    /// this is the one question about *presence* that something outside the
    /// module has to answer -- because the data the method reads lives in the
    /// pool, not in this set.
    pub(crate) fn wants(&self, me: Me) -> bool {
        self.enabled.contains(&me)
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
        SET.iter()
            .copied()
            .filter(|m| self.enabled.contains(m))
            .collect()
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
            Me::Includes => "__m_includes",
            Me::StartsWith => "__m_starts_with",
            Me::EndsWith => "__m_ends_with",
            Me::Substr => "__m_substr",
            Me::Split => "__m_split",
            Me::Decode => "__m_decode",
            Me::Encode => "__m_encode",
            Me::LowerCp => "__m_lower_cp",
            Me::ToLowerCase => "__m_to_lower_case",
            Me::Slice => "__m_slice",
            Me::SliceFrom => "__m_slice_from",
            Me::SliceCore => "__m_slice_core",
            Me::Replace => "__m_replace",
            Me::ReplaceAll => "__m_replace_all",
            Me::ObjKeys => "__m_obj_keys",
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
            ("includes", 1) => Some(Me::Includes),
            ("startsWith", 1) => Some(Me::StartsWith),
            ("endsWith", 1) => Some(Me::EndsWith),
            ("split", 1) => Some(Me::Split),
            ("toLowerCase", 0) => Some(Me::ToLowerCase),
            ("slice", 2) => Some(Me::Slice),
            ("slice", 1) => Some(Me::SliceFrom),
            ("replace", 2) => Some(Me::Replace),
            ("replaceAll", 2) => Some(Me::ReplaceAll),
            ("__keys", 0) => Some(Me::ObjKeys),
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

    /// Whether the receiver is an Object record. The third receiver kind: the
    /// call site used to test "array, else string", which sent an object
    /// receiver down the property path and into the String refusal --
    /// `Object.keys({})` trapped on its first day for exactly that.
    pub(crate) fn receiver_is_object(self) -> bool {
        matches!(self, Me::ObjKeys)
    }

    /// Whether this method's body reaches into the array set, which the
    /// *array* gate controls. See [`Ctx::array_base`].
    pub(crate) fn needs_arrays(self) -> bool {
        matches!(self, Me::Push | Me::MapBound | Me::Split | Me::ObjKeys)
    }

    /// What this method's body calls, so [`Plan`] can pull them in.
    fn helpers(self) -> Vec<Me> {
        match self {
            Me::Trim => vec![Me::WsWidth],
            Me::IndexOf => vec![Me::Units],
            // No `Units`: a Boolean has no position to report, so none of the
            // three needs the byte-offset-to-code-unit conversion `indexOf`
            // exists to do. This is the whole reason they are not written as
            // `indexOf(t) !== -1`.
            Me::Includes | Me::StartsWith | Me::EndsWith | Me::Substr => Vec::new(),
            Me::Split => vec![Me::Substr],
            Me::ToLowerCase => vec![Me::Decode, Me::Encode, Me::LowerCp],
            // Flat, like `ToLowerCase`'s: `want` pulls one level of helpers.
            Me::Slice | Me::SliceFrom => vec![Me::SliceCore, Me::Units, Me::Substr],
            Me::SliceCore => vec![Me::Units, Me::Substr],
            Me::Decode | Me::Encode | Me::LowerCp => Vec::new(),
            Me::Replace | Me::ReplaceAll | Me::ObjKeys => Vec::new(),
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
        Me::Includes => (values(2), values(1), includes()),
        Me::StartsWith => (values(2), values(1), affix(Affix::Start)),
        Me::EndsWith => (values(2), values(1), affix(Affix::End)),
        Me::Substr => (
            vec![ValType::I32, ValType::I32, ValType::I32],
            vec![ValType::I32],
            substr(ctx),
        ),
        Me::Split => (values(2), values(1), split(ctx)),
        Me::Decode => (
            vec![ValType::I32],
            vec![ValType::I32, ValType::I32],
            decode(),
        ),
        Me::Encode => (
            vec![ValType::I32, ValType::I32],
            vec![ValType::I32],
            encode(),
        ),
        Me::LowerCp => (vec![ValType::I32], vec![ValType::I32], lower_cp(ctx)),
        Me::ToLowerCase => (values(1), values(1), to_lower_case(ctx)),
        Me::Slice => (values(3), values(1), slice(ctx, true)),
        Me::SliceFrom => (values(2), values(1), slice(ctx, false)),
        Me::SliceCore => (vec![i32_, i32_, i32_], vec![i32_], slice_core(ctx)),
        Me::Replace => (values(3), values(1), replace(ctx, Reach::First)),
        Me::ReplaceAll => (values(3), values(1), replace(ctx, Reach::All)),
        Me::ObjKeys => (values(1), values(1), obj_keys(ctx)),
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

    let inner = vec![Ins::LocalGet(out)];
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
/// Skip the four haystack bytes at `i` when none of them is the needle's
/// first byte, which is what most windows of a long haystack are: one
/// `i32.load` and the has-zero-byte trick against `first * 0x01010101`
/// -- `(x - 0x01010101) & ~x & 0x80808080` with `x` the xor -- spelled with
/// `(a|b)-(a&b)` for the xor and `-1 - x` for the not, since the instruction
/// set has neither. Emitted at the top of the position loop, after the
/// bound check: when the window is clear the loop continues at `i + 4`
/// (the bound check catches an overshoot); when it is not, the caller's
/// byte verify runs at `i` as before. A 128 KiB miss went from ~36 to under
/// 10 steps a character; text whose lines all start with the first byte
/// falls back to the old price, no worse.
///
/// `p` holds `first * 0x01010101`, computed once by the caller; `w` is a
/// scratch local.
fn skip_clear_window(b: &mut Vec<Ins>, h: u32, i: u32, hl: u32, p: u32, w: u32) {
    // if i + 4 <= hl, spelled !(hl < i + 4)
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(Ins::I32LtU);
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    // w = load32(h + 4 + i)
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load(0, 4));
    b.push(Ins::LocalSet(w));
    // x = (w | p) - (w & p)   -- the xor
    b.push(Ins::LocalGet(w));
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32Or);
    b.push(Ins::LocalGet(w));
    b.push(Ins::LocalGet(p));
    b.push(Ins::I32And);
    b.push(Ins::I32Sub);
    b.push(Ins::LocalSet(w));
    // z = (x - 0x01010101) & (-1 - x) & 0x80808080
    b.push(Ins::LocalGet(w));
    b.push(Ins::I32Const(0x0101_0101));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(-1));
    b.push(Ins::LocalGet(w));
    b.push(Ins::I32Sub);
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x8080_8080u32 as i32));
    b.push(Ins::I32And);
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    // continue the position loop: `if` is 0, this `if` 1, the loop 2
    b.push(Ins::Br(2));
    b.push(Ins::End);
    b.push(Ins::End);
}

/// `p = first_byte(needle) * 0x01010101`, for [`skip_clear_window`].
fn first_byte_pattern(b: &mut Vec<Ins>, nd: u32, p: u32) {
    b.push(Ins::LocalGet(nd));
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Const(0x0101_0101));
    b.push(Ins::I32Mul);
    b.push(Ins::LocalSet(p));
}

fn index_of(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let h = f.local(ValType::I32);
    let nd = f.local(ValType::I32);
    let hl = f.local(ValType::I32);
    let nl = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let j = f.local(ValType::I32);
    let ok = f.local(ValType::I32);
    let p = f.local(ValType::I32);
    let w = f.local(ValType::I32);

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

    first_byte_pattern(b, nd, p);
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32LtU);
    b.push(Ins::BrIf(1));
    skip_clear_window(b, h, i, hl, p, w);

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
    let found = vec![
        Ins::LocalGet(h),
        Ins::LocalGet(i),
        ctx.me(Me::Units),
        Ins::F64ConvertI32S,
    ];
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
    let inner = vec![Ins::F64Const(v as f64)];
    let mut out = Vec::new();
    box_number(&inner, &mut out);
    b.extend(out);
}

/// Which end of the haystack an affix test compares.
///
/// One function for `startsWith` and `endsWith`, because they differ in a
/// single addend: the offset the comparison starts at. Writing them separately
/// would duplicate the length guard and the byte loop to express that.
#[derive(Clone, Copy)]
enum Affix {
    Start,
    End,
}

/// `s.includes(t)` -- ECMA-262 22.1.3.8.
///
/// # Why a byte comparison is the right one
///
/// UTF-8 is self-synchronising and prefix-free: a multi-byte sequence's
/// continuation bytes are all `10xxxxxx` and can never start one, so a byte
/// sequence matches at some offset **iff** the corresponding code-point
/// sequence does. There is no possibility of matching the tail of one
/// character and the head of the next, which is the failure that makes
/// byte-level search wrong for encodings like Shift-JIS. So this needs no
/// decoding, and the result is exact rather than an approximation.
///
/// Compare `.length`, where the same reasoning does **not** apply: a count of
/// characters is not a count of bytes, which is why that one decodes.
fn includes() -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let h = f.local(ValType::I32);
    let nd = f.local(ValType::I32);
    let hl = f.local(ValType::I32);
    let nl = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let j = f.local(ValType::I32);
    let ok = f.local(ValType::I32);
    let p = f.local(ValType::I32);
    let w = f.local(ValType::I32);

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

    // 22.1.3.8 step 8 with an empty search string: `IsStringWellFormedUnicode`
    // aside, every string contains the empty one.
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    const_bool(true, b);
    b.push(Ins::Return);
    b.push(Ins::End);

    // Tested before the subtraction below, which would otherwise wrap.
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    const_bool(false, b);
    b.push(Ins::Return);
    b.push(Ins::End);

    first_byte_pattern(b, nd, p);
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32LtU);
    b.push(Ins::BrIf(1));
    skip_clear_window(b, h, i, hl, p, w);

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
    b.push(Ins::Br(2));
    b.push(Ins::End);
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(j));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(ok));
    b.push(Ins::If(BlockType::Empty));
    const_bool(true, b);
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    const_bool(false, b);
    f
}

/// `s.startsWith(t)` and `s.endsWith(t)` -- ECMA-262 22.1.3.23 and 22.1.3.7.
///
/// One comparison at a fixed offset, so there is no outer loop: the two
/// differ only in whether that offset is `0` or `haystack - needle`. Byte
/// comparison is exact here for the reason [`includes`] gives, plus a second
/// one that matters only for `endsWith`: because UTF-8 is self-synchronising,
/// an offset that is a suffix boundary in bytes is one in characters too, so
/// this cannot match starting halfway through a character.
fn affix(which: Affix) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let h = f.local(ValType::I32);
    let nd = f.local(ValType::I32);
    let hl = f.local(ValType::I32);
    let nl = f.local(ValType::I32);
    let at = f.local(ValType::I32);
    let j = f.local(ValType::I32);

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

    // A needle longer than the haystack cannot be either affix, and the test
    // has to come before the subtraction for `End` or it wraps.
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    const_bool(false, b);
    b.push(Ins::Return);
    b.push(Ins::End);

    match which {
        Affix::Start => {
            b.push(Ins::I32Const(0));
        }
        Affix::End => {
            b.push(Ins::LocalGet(hl));
            b.push(Ins::LocalGet(nl));
            b.push(Ins::I32Sub);
        }
    }
    b.push(Ins::LocalSet(at));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(j));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(at));
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
    const_bool(false, b);
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(j));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    const_bool(true, b);
    f
}

/// `substr(p, start, n) -> i32`: a freshly allocated string holding `n` bytes
/// of the string body at `p`, from byte offset `start`.
///
/// A helper rather than inline code because `split` emits it once per piece
/// plus once for the tail, and because the next string method that returns a
/// piece of its receiver -- `slice`, `replace` -- will want exactly this.
///
/// Byte offsets, not code-unit ones. Every caller so far has found its offsets
/// by matching a *separator*, and a separator boundary is a character boundary
/// in UTF-8 by construction, so no decoding is needed to know the slice is
/// well-formed. A future caller that takes offsets from the script -- `slice`
/// does -- cannot reuse this without converting first, and that conversion is
/// `Me::Units` run backwards.
fn substr(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(3);
    let p = 0;
    let start = 1;
    let n = 2;
    let out = f.local(ValType::I32);
    let i = f.local(ValType::I32);

    let b = &mut f.body;
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(out));
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Store(ALIGN_WORD, 0));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(n));
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

    b.push(Ins::LocalGet(out));
    f
}

/// `s.split(sep)` -- ECMA-262 22.1.3.23, for a non-empty string separator.
///
/// Third in the second demand survey: `.split(` appears in 34 of 82 downstream
/// scripts, 129 times. What they split on was counted too, and it decided the
/// shape of this: 54 of the 129 are `"\n"`, the rest are other short literals,
/// and **`split("")` appears zero times**.
///
/// # The empty separator traps, and that is forced rather than chosen
///
/// ECMA-262 splits on an empty separator into UTF-16 **code units**, so
/// `"😀".split("")` is two lone surrogates. This engine's strings are UTF-8
/// and a lone surrogate is not representable in it -- there is no byte
/// sequence that means one. So conformance here is not deferred work, it is
/// unreachable from this representation, and the two alternatives are both
/// worse than a trap: splitting by *code point* instead would be a silent
/// wrong answer for exactly the inputs that make the case interesting, and
/// returning the whole string would be a silent wrong answer for all of them.
///
/// Zero uses in the corpus is what makes a trap affordable. It is not what
/// makes it right.
///
/// # No separator found
///
/// `["whole"]`, per step 14 -- which falls out of the loop rather than being
/// special-cased: nothing matches, so the tail push after the loop is the only
/// push that happens.
fn split(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let h = f.local(ValType::I32);
    let nd = f.local(ValType::I32);
    let hl = f.local(ValType::I32);
    let nl = f.local(ValType::I32);
    let a = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let j = f.local(ValType::I32);
    let start = f.local(ValType::I32);
    let ok = f.local(ValType::I32);
    let p = f.local(ValType::I32);
    let w = f.local(ValType::I32);

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

    // See the doc comment: unrepresentable rather than unimplemented -- and
    // since 2026-08-30 a *named* capability refusal rather than a bare stop.
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    if let Some(names) = ctx.refusal_names {
        record_named_fault(names.empty_separator, FAULT_CAPABILITY, b);
    }
    b.push(Ins::Unreachable);
    b.push(Ins::End);

    // `Ar::New` takes a capacity. Zero, because the piece count is not known
    // until the scan has run and guessing it would either over-allocate for
    // the common case -- 54 of the corpus's 129 splits are on `"\n"` and most
    // lines are short -- or under-allocate and grow anyway.
    b.push(Ins::I32Const(0));
    b.push(ctx.arr(Ar::New));
    b.push(Ins::LocalSet(a));

    // Skipped entirely when the separator is longer than the string, which is
    // also what keeps `hl - nl` from wrapping.
    // `nl <= hl`, written as `!(hl < nl)`: this instruction set has no
    // unsigned `<=`, and inverting the one it has is cheaper than widening it.
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32LtU);
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    first_byte_pattern(b, nd, p);
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32LtU);
    b.push(Ins::BrIf(1));
    skip_clear_window(b, h, i, hl, p, w);

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
    b.push(Ins::Br(2));
    b.push(Ins::End);
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(j));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(ok));
    b.push(Ins::If(BlockType::Empty));
    // One piece: from `start` up to this match.
    b.push(Ins::LocalGet(a));
    let piece = vec![
        Ins::LocalGet(h),
        Ins::LocalGet(start),
        Ins::LocalGet(i),
        Ins::LocalGet(start),
        Ins::I32Sub,
        ctx.me(Me::Substr),
    ];
    let mut boxed = Vec::new();
    box_string(&piece, &mut boxed);
    b.extend(boxed);
    b.push(ctx.arr(Ar::Push));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalSet(start));
    b.push(Ins::End);
    // The other arm, as a second `if` rather than an `else`: this instruction
    // set has no `else`, and a matched separator has already moved `i` past
    // itself, so the two arms are genuinely exclusive on `ok` alone.
    b.push(Ins::LocalGet(ok));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::End);
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::End);

    // The tail, always: `"a,".split(",")` is `["a", ""]`, and `"x".split(",")`
    // is `["x"]`. Both are this push and nothing else.
    b.push(Ins::LocalGet(a));
    let tail = vec![
        Ins::LocalGet(h),
        Ins::LocalGet(start),
        Ins::LocalGet(hl),
        Ins::LocalGet(start),
        Ins::I32Sub,
        ctx.me(Me::Substr),
    ];
    let mut boxed = Vec::new();
    box_string(&tail, &mut boxed);
    b.extend(boxed);
    b.push(ctx.arr(Ar::Push));

    let inner = vec![Ins::LocalGet(a)];
    let mut out = Vec::new();
    box_array(&inner, &mut out);
    f.body.extend(out);
    f
}

/// `decode(p) -> (cp, width)`: the code point at byte address `p`, and how
/// many bytes it took.
///
/// A helper because `toLowerCase` needs it twice per character -- once to read
/// and once to know how far to step -- and because the next method that walks
/// characters rather than bytes will want the same. `Me::Units` only needs the
/// *width*, which is why it does not share this: counting leading bytes is
/// cheaper than decoding them.
///
/// Well-formedness is assumed, not checked. Every string in this engine came
/// from a literal, a concatenation of literals, or a host string, and all
/// three are UTF-8 by construction -- there is no route by which an
/// ill-formed one reaches here, so validating would cost every character to
/// defend against nothing.
fn decode() -> FnBuild {
    let mut f = FnBuild::new(1);
    let b0 = f.local(ValType::I32);
    let cp = f.local(ValType::I32);
    let w = f.local(ValType::I32);

    let b = &mut f.body;
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 0));
    b.push(Ins::LocalSet(b0));

    // 1 byte: 0xxxxxxx
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(b0));
    b.push(Ins::LocalSet(cp));
    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(w));
    b.push(Ins::End);

    // 2 bytes: 110xxxxx 10xxxxxx
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xc0));
    b.push(Ins::I32GeU);
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xe0));
    b.push(Ins::I32LtU);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0x1f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(6));
    b.push(Ins::I32Shl);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Or);
    b.push(Ins::LocalSet(cp));
    b.push(Ins::I32Const(2));
    b.push(Ins::LocalSet(w));
    b.push(Ins::End);

    // 3 bytes: 1110xxxx 10xxxxxx 10xxxxxx
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xe0));
    b.push(Ins::I32GeU);
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xf0));
    b.push(Ins::I32LtU);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0x0f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(12));
    b.push(Ins::I32Shl);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(6));
    b.push(Ins::I32Shl);
    b.push(Ins::I32Or);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Or);
    b.push(Ins::LocalSet(cp));
    b.push(Ins::I32Const(3));
    b.push(Ins::LocalSet(w));
    b.push(Ins::End);

    // 4 bytes: 11110xxx and three continuations.
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0xf0));
    b.push(Ins::I32GeU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(b0));
    b.push(Ins::I32Const(0x07));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(18));
    b.push(Ins::I32Shl);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 1));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(12));
    b.push(Ins::I32Shl);
    b.push(Ins::I32Or);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 2));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(6));
    b.push(Ins::I32Shl);
    b.push(Ins::I32Or);
    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Load8U(0, 3));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Or);
    b.push(Ins::LocalSet(cp));
    b.push(Ins::I32Const(4));
    b.push(Ins::LocalSet(w));
    b.push(Ins::End);

    b.push(Ins::LocalGet(cp));
    b.push(Ins::LocalGet(w));
    f
}

/// `encode(dst, cp) -> width`: write `cp` as UTF-8 at `dst`, return the bytes
/// written.
fn encode() -> FnBuild {
    let mut f = FnBuild::new(2);
    let dst = 0;
    let cp = 1;

    let b = &mut f.body;
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Store8(0, 0));
    b.push(Ins::I32Const(1));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(0x800));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(6));
    b.push(Ins::I32ShrU);
    b.push(Ins::I32Const(0xc0));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 0));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 1));
    b.push(Ins::I32Const(2));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(0x10000));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(12));
    b.push(Ins::I32ShrU);
    b.push(Ins::I32Const(0xe0));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 0));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(6));
    b.push(Ins::I32ShrU);
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 1));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 2));
    b.push(Ins::I32Const(3));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(18));
    b.push(Ins::I32ShrU);
    b.push(Ins::I32Const(0xf0));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 0));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(12));
    b.push(Ins::I32ShrU);
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 1));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(6));
    b.push(Ins::I32ShrU);
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 2));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(0x3f));
    b.push(Ins::I32And);
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32Or);
    b.push(Ins::I32Store8(0, 3));
    b.push(Ins::I32Const(4));
    f
}

/// `lower_cp(cp) -> cp`: the Unicode simple lowercase mapping, by binary
/// search over the run table in the data segment.
///
/// The table is `crate::case::RUNS` as fixed 12-byte entries -- `u32 start`,
/// `u32 len`, `i32 delta` -- placed by the emitter and pointed at by
/// [`Ctx::case_table`]. Fixed width is what lets the search index by
/// multiplication; `crate::case` records the two compact encodings measured
/// and rejected, and what they would have saved.
///
/// A code point with no entry is returned unchanged, which is most of them:
/// 1460 of 1 114 112 have a mapping at all. Chinese, emoji, punctuation and
/// already-lowercase Latin all take that path, and none of them traps -- which
/// is criterion ② of `plan/design-case-mapping-decision.md` and the reason
/// "trap on anything non-ASCII" was rejected.
fn lower_cp(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(1);
    let lo = f.local(ValType::I32);
    let hi = f.local(ValType::I32);
    let mid = f.local(ValType::I32);
    let at = f.local(ValType::I32);
    let start = f.local(ValType::I32);

    let b = &mut f.body;
    b.push(Ins::I32Const(ctx.case_runs as i32));
    b.push(Ins::LocalSet(hi));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(lo));
    b.push(Ins::LocalGet(hi));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));

    b.push(Ins::LocalGet(lo));
    b.push(Ins::LocalGet(hi));
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(2));
    b.push(Ins::I32DivS);
    b.push(Ins::LocalSet(mid));
    b.push(Ins::LocalGet(mid));
    b.push(Ins::I32Const(12));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Const(ctx.case_table));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(at));
    b.push(Ins::LocalGet(at));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(start));

    // cp < start -> search left
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(start));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(mid));
    b.push(Ins::LocalSet(hi));
    // `Br(1)`, not `Br(2)`: from inside this `if` the labels are 0 = the
    // `if`, 1 = the loop, 2 = the block around it. Branching to a *loop* label
    // jumps to its top, which is the "search left again" this arm means;
    // `Br(2)` would leave the search entirely and answer "no mapping" for
    // every code point below the midpoint -- which is what it did, and what
    // made `"HELLO"` lowercase to `"HELLO"`.
    b.push(Ins::Br(1));
    b.push(Ins::End);

    // cp < start + len -> found
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(start));
    b.push(Ins::LocalGet(at));
    b.push(Ins::I32Load(ALIGN_WORD, 4));
    b.push(Ins::I32Add);
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(at));
    b.push(Ins::I32Load(ALIGN_WORD, 8));
    b.push(Ins::I32Add);
    b.push(Ins::Return);
    b.push(Ins::End);

    // otherwise search right
    b.push(Ins::LocalGet(mid));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(lo));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(0));
    f
}

/// `s.toLowerCase()` -- ECMA-262 22.1.3.29, Unicode simple case mapping.
///
/// Fourth in the second demand survey at 25 of 82 scripts and 67 uses -- and
/// all 67 are `to_lower`, with `to_upper` at zero, which is why this ships
/// alone rather than as a pair.
///
/// # The output buffer
///
/// Allocated at `hl + hl/2`, not `hl`, because two code points lowercase to a
/// *longer* UTF-8 sequence: U+023A and U+023E are two bytes and map to three.
/// Everything else keeps or shrinks its width, so 3/2 is a bound rather than a
/// guess, and it buys a single pass -- the alternative is decoding every
/// character twice, once to measure and once to write. The record's length is
/// written at the end from what was actually produced, so the slack is heap
/// the bump allocator never hands out again and nothing else can observe.
fn to_lower_case(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let h = f.local(ValType::I32);
    let hl = f.local(ValType::I32);
    let out = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let o = f.local(ValType::I32);
    let cp = f.local(ValType::I32);
    let w = f.local(ValType::I32);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(h));

    let b = &mut f.body;
    b.push(Ins::LocalGet(h));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(hl));

    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32ShrU);
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(out));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));

    // ASCII first: a byte under 0x80 is its own code point, lowers by
    // arithmetic (`+ 32` exactly when it is `A`..`Z`, as `(b - 65) <u 26`)
    // and is one byte out. The decode / map / encode round trip below is
    // for everything else. 393 steps a character on ASCII text before this
    // (2026-08-30, a 729 KB corpus lowercased downstream).
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::LocalTee(cp));
    b.push(Ins::I32Const(0x80));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(cp));
    b.push(Ins::LocalGet(cp));
    b.push(Ins::I32Const(65));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(26));
    b.push(Ins::I32LtU);
    b.push(Ins::I32Const(5));
    b.push(Ins::I32Shl);
    b.push(Ins::I32Add);
    b.push(Ins::I32Store8(0, 4));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(o));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    // `if` is 0, the loop 1
    b.push(Ins::Br(1));
    b.push(Ins::End);

    b.push(Ins::LocalGet(h));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(ctx.me(Me::Decode));
    b.push(Ins::LocalSet(w));
    b.push(Ins::LocalSet(cp));

    b.push(Ins::LocalGet(out));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(cp));
    b.push(ctx.me(Me::LowerCp));
    b.push(ctx.me(Me::Encode));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(o));

    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(w));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Store(ALIGN_WORD, 0));

    let inner = vec![Ins::LocalGet(out)];
    let mut boxed = Vec::new();
    box_string(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `s.slice(start[, end])` -- ECMA-262 22.1.3.21.
///
/// Fourth in the migration surveys and the one every wave-1 group asked for
/// by name: `sub_string(a, b)` in rh became `slice(a, b)` in the mapping, and
/// `test_harness.bounded_record_text` truncates evidence with it.
///
/// Each index is a Number (the spec's ToIntegerOrInfinity on other types is
/// not done: a non-Number traps in `unbox_number`, which is this engine's
/// standing narrowing for method arguments). NaN is 0; the value is clamped
/// to `[-len, len]` *before* truncation so the truncation cannot trap, and
/// the clamp changes nothing inside that range; a negative index counts from
/// the end. Positions are UTF-16 code units, as `length` counts them.
///
/// `has_end` picks the two-argument body; the one-argument form takes `end =
/// length`. Both are a few instructions around [`Me::SliceCore`].
fn slice(ctx: &Ctx, has_end: bool) -> FnBuild {
    let mut f = FnBuild::new(if has_end { 3 * WIDTH } else { 2 * WIDTH });
    let h = f.local(ValType::I32);
    let from = f.local(ValType::I32);
    let to = f.local(ValType::I32);
    let r = f.local(ValType::F64);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(h));

    let b = &mut f.body;
    // One relative index. NaN -> 0. A non-negative index needs no length at
    // all: the core treats a position past the end as the end, so the walk
    // stops at the index, not at the string's end -- `s.slice(0, 10)` on a
    // 1000-char string used to cost 78 000 steps for that walk. Only a
    // negative index counts from the end and has to know it, and it is
    // counted then, not before.
    let relative = |b: &mut Vec<Ins>, arg: u32, into: u32| {
        unbox_number(arg, b);
        b.push(Ins::LocalSet(r));
        // NaN -> 0; then clamp into i32 range so the truncation cannot trap.
        b.push(Ins::LocalGet(r));
        b.push(Ins::LocalGet(r));
        b.push(Ins::F64Ne);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::F64Const(0.0));
        b.push(Ins::LocalSet(r));
        b.push(Ins::End);
        b.push(Ins::LocalGet(r));
        b.push(Ins::F64Const(2_147_483_647.0));
        b.push(Ins::F64Gt);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::F64Const(2_147_483_647.0));
        b.push(Ins::LocalSet(r));
        b.push(Ins::End);
        b.push(Ins::LocalGet(r));
        b.push(Ins::F64Const(-2_147_483_647.0));
        b.push(Ins::F64Lt);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::F64Const(-2_147_483_647.0));
        b.push(Ins::LocalSet(r));
        b.push(Ins::End);
        // Truncate first (ToIntegerOrInfinity: -0.5 is 0, not "negative"),
        // then a negative integer counts from the end -- and only then is
        // the length counted: a non-negative index needs no length at all,
        // because the core treats a position past the end as the end.
        // `s.slice(0, 10)` on a 1000-char string used to walk all 1000.
        b.push(Ins::LocalGet(r));
        b.push(Ins::I32TruncF64S);
        b.push(Ins::LocalSet(into));
        b.push(Ins::LocalGet(into));
        b.push(Ins::I32Const(0));
        b.push(Ins::I32LtS);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::LocalGet(h));
        b.push(Ins::LocalGet(h));
        b.push(Ins::I32Load(ALIGN_WORD, 0));
        b.push(ctx.me(Me::Units));
        b.push(Ins::LocalGet(into));
        b.push(Ins::I32Add);
        b.push(Ins::LocalSet(into));
        b.push(Ins::LocalGet(into));
        b.push(Ins::I32Const(0));
        b.push(Ins::I32LtS);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::I32Const(0));
        b.push(Ins::LocalSet(into));
        b.push(Ins::End);
        b.push(Ins::End);
    };
    relative(b, WIDTH, from);
    if has_end {
        relative(b, 2 * WIDTH, to);
    } else {
        b.push(Ins::I32Const(i32::MAX));
        b.push(Ins::LocalSet(to));
    }

    let inner = vec![
        Ins::LocalGet(h),
        Ins::LocalGet(from),
        Ins::LocalGet(to),
        ctx.me(Me::SliceCore),
    ];
    let mut boxed = Vec::new();
    box_string(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `(record, from, to) -> record`: the byte offsets of two code-unit
/// positions, found in one pass over the bytes, then [`Me::Substr`].
///
/// A lead byte starts a code unit; a 4-byte sequence is two. A position that
/// falls on the second unit of such a pair is a lone surrogate, which no
/// UTF-8 byte sequence means, so it traps: unrepresentable rather than
/// unimplemented, as `split("")` says of itself. `from >= to` is the empty
/// string before any byte is looked at (step 11 of the spec).
fn slice_core(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(3);
    let h = 0;
    let from = 1;
    let to = 2;
    let hl = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let u = f.local(ValType::I32);
    let bf = f.local(ValType::I32);
    let bt = f.local(ValType::I32);
    let byte = f.local(ValType::I32);
    let w = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(from));
    b.push(Ins::LocalGet(to));
    b.push(Ins::I32LtS);
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(h));
    b.push(Ins::I32Const(0));
    b.push(Ins::I32Const(0));
    b.push(ctx.me(Me::Substr));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(h));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(hl));
    // Past the end unless the walk finds them: `to == length` is the
    // common case and never meets a lead byte.
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalSet(bf));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalSet(bt));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(h));
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
    b.push(Ins::LocalGet(u));
    b.push(Ins::LocalGet(from));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalSet(bf));
    b.push(Ins::End);
    b.push(Ins::LocalGet(u));
    b.push(Ins::LocalGet(to));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalSet(bt));
    // Found the end: leave the walk. From inside two `if`s the loop is
    // depth 2 and the block around it depth 3.
    b.push(Ins::Br(3));
    b.push(Ins::End);
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(0xf0));
    b.push(Ins::I32GeU);
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(w));
    b.push(Ins::LocalGet(w));
    b.push(Ins::I32Const(2));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    // A boundary that falls between the two code units of a surrogate pair
    // has no byte position in UTF-8 (see the doc comment); a named
    // capability refusal since 2026-08-30.
    for edge in [from, to] {
        b.push(Ins::LocalGet(u));
        b.push(Ins::I32Const(1));
        b.push(Ins::I32Add);
        b.push(Ins::LocalGet(edge));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        if let Some(names) = ctx.refusal_names {
            record_named_fault(names.surrogate_boundary, FAULT_CAPABILITY, b);
        }
        b.push(Ins::Unreachable);
        b.push(Ins::End);
    }
    b.push(Ins::End);
    b.push(Ins::LocalGet(u));
    b.push(Ins::LocalGet(w));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(u));
    b.push(Ins::End);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(bf));
    b.push(Ins::LocalGet(bt));
    b.push(Ins::LocalGet(bf));
    b.push(Ins::I32Sub);
    b.push(ctx.me(Me::Substr));
    f
}

/// Whether an occurrence loop stops after the first match.
#[derive(Clone, Copy, PartialEq)]
enum Reach {
    First,
    All,
}

/// `s.replace(from, to)` and `s.replaceAll(from, to)` -- ECMA-262 22.1.3.19
/// and 22.1.3.20, for string patterns.
///
/// # Both, because the corpus means one and JavaScript authors write the other
///
/// `.replace(` is 17% of downstream scripts but **142 uses**, the widest gap
/// between "how many are blocked" and "how much it hurts" in the second
/// survey. Every one of them looks like `.replace("\r\n", "\n")` -- normalising
/// line endings, which means *every* occurrence.
///
/// In JavaScript that is `replaceAll`. `replace` with a string pattern
/// replaces **only the first** match, which is a difference a script written
/// against the corpus's habits would meet as a silent wrong answer. So both
/// ship, each with its own name and its own meaning.
///
/// # They share this function and **not** the bytes
///
/// [`Reach`] is decided at compile time, so each name emits its own copy.
/// Measured: `replace` is 525 bytes and adding `replaceAll` costs 515 more --
/// the second is not "nearly free", which is what this comment claimed before
/// the number existed. **Sharing a Rust function shares maintenance, not
/// emitted code**, and in a prefab layer those are different currencies.
///
/// Merging them into one emitted function with a runtime flag was considered
/// and not done: it would save 515 bytes for a program that uses both, and
/// charge every program that uses only one for a parameter and a branch it
/// cannot take. The gate is per method precisely so the common case pays for
/// what it uses.
///
/// # The empty pattern
///
/// `"abc".replaceAll("", "-")` is `"-a-b-c-"` in ECMA-262: the empty string
/// matches at every position including the ends. Unlike `split("")`, that is
/// representable here -- no lone surrogates are involved -- so it is
/// implemented rather than refused. `replace("", "-")` inserts once, at 0.
fn replace(ctx: &Ctx, reach: Reach) -> FnBuild {
    let mut f = FnBuild::new(3 * WIDTH);
    let h = f.local(ValType::I32);
    let nd = f.local(ValType::I32);
    let rp = f.local(ValType::I32);
    let hl = f.local(ValType::I32);
    let nl = f.local(ValType::I32);
    let rl = f.local(ValType::I32);
    let out = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let o = f.local(ValType::I32);
    let j = f.local(ValType::I32);
    let ok = f.local(ValType::I32);
    let done = f.local(ValType::I32);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(h));
    unbox_string(WIDTH, &mut f.body);
    f.body.push(Ins::LocalSet(nd));
    unbox_string(2 * WIDTH, &mut f.body);
    f.body.push(Ins::LocalSet(rp));

    let b = &mut f.body;
    for (src, dst) in [(h, hl), (nd, nl), (rp, rl)] {
        b.push(Ins::LocalGet(src));
        b.push(Ins::I32Load(ALIGN_WORD, 0));
        b.push(Ins::LocalSet(dst));
    }

    // The output cannot exceed one replacement per input byte plus one for the
    // empty-pattern match at each position, plus the tail: `(hl + 1) * rl + hl`
    // is a bound rather than a guess, and the record's length is written from
    // what was produced. Over-allocation is bump-heap slack nothing observes.
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(rl));
    b.push(Ins::I32Mul);
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(out));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    // `i > hl`, written as `hl < i`: no unsigned `>` here, and swapping the
    // operands of the `<` there is says the same thing. The bound is `>` and
    // not `>=` because an empty pattern matches at `hl` too -- the position
    // after the last byte is a real match site.
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32LtU);
    b.push(Ins::BrIf(1));

    // Does the pattern match here? An empty pattern matches everywhere, and
    // one that would run past the end does not.
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(ok));
    b.push(Ins::LocalGet(done));
    b.push(Ins::I32Eqz);
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Add);
    b.push(Ins::I32LtU);
    b.push(Ins::I32Eqz);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
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
    b.push(Ins::Br(2));
    b.push(Ins::End);
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(j));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(ok));
    b.push(Ins::If(BlockType::Empty));
    // Copy the replacement.
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(j));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(j));
    b.push(Ins::LocalGet(rl));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(rp));
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Store8(0, 4));
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(j));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::LocalGet(o));
    b.push(Ins::LocalGet(rl));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(o));
    if reach == Reach::First {
        b.push(Ins::I32Const(1));
        b.push(Ins::LocalSet(done));
    }
    // An empty pattern must still advance, or the loop never ends: copy the
    // character it matched before and step past it.
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Store8(0, 4));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(o));
    b.push(Ins::End);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::End);
    // The non-empty arm, as a second `if`: this instruction set has no `else`.
    b.push(Ins::LocalGet(nl));
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(nl));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::End);
    b.push(Ins::End);

    // No match here: copy one byte and step.
    b.push(Ins::LocalGet(ok));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Store8(0, 4));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(o));
    b.push(Ins::End);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::End);

    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Store(ALIGN_WORD, 0));

    let inner = vec![Ins::LocalGet(out)];
    let mut boxed = Vec::new();
    box_string(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `Object.keys(o)` -- ECMA-262 20.1.2.17.
///
/// A record here holds only own enumerable string keys, so the answer is
/// every entry's key in insertion order and nothing has to be filtered. The
/// keys are the record's own String pointers -- literals in the pool or
/// strings the script built -- so the new array shares them rather than
/// copying: a String is immutable, and `__str_eq` compares bytes.
///
/// Shape is `map_bound`'s without the callback: new array sized to the
/// count, one push per entry, box the array.
fn obj_keys(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let o = f.local(ValType::I32);
    let entries = f.local(ValType::I32);
    let out = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let n = f.local(ValType::I32);

    unbox_object(0, &mut f.body);
    f.body.push(Ins::LocalSet(o));

    let b = &mut f.body;
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_LEN));
    b.push(Ins::LocalSet(n));
    b.push(Ins::LocalGet(o));
    b.push(Ins::I32Load(ALIGN_WORD, OBJ_ENTRIES));
    b.push(Ins::LocalSet(entries));
    b.push(Ins::LocalGet(n));
    b.push(ctx.arr(Ar::New));
    b.push(Ins::LocalSet(out));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));

    b.push(Ins::LocalGet(out));
    let key = vec![
        Ins::LocalGet(entries),
        Ins::LocalGet(i),
        Ins::I32Const(ENTRY_BYTES),
        Ins::I32Mul,
        Ins::I32Add,
        Ins::I32Load(ALIGN_WORD, ENTRY_KEY),
    ];
    let mut boxed = Vec::new();
    box_string(&key, &mut boxed);
    b.extend(boxed);
    b.push(ctx.arr(Ar::Push));

    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    let inner = vec![Ins::LocalGet(out)];
    let mut boxed = Vec::new();
    box_array(&inner, &mut boxed);
    f.body.extend(boxed);
    f
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

    let inner = vec![
        Ins::LocalGet(a),
        Ins::I32Load(ALIGN_WORD, ARR_LEN),
        Ins::F64ConvertI32S,
    ];
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

    let inner = vec![Ins::LocalGet(out)];
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
