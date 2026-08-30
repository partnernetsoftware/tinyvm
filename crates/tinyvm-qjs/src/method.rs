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
    self, BlockType, Ins, TAG_NULL, TAG_NUMBER, TAG_UNDEFINED, ValType, WIDTH, box_number,
    box_string, load_local, unbox_array, unbox_number, unbox_string,
};
// `map`'s prefab is the one function that builds an array and calls back into
// a function value; `box_function` is variant A's and B's property read.
use super::array::{ARR_ELEMS, ARR_LEN, Ar, ELEM_BYTES, ELEM_PAYLOAD, ELEM_TAG};
use super::ast::m1 as ast;
use super::repr::{box_array, box_bool, const_bool, const_undefined, unbox_object};
use super::runtime::{
    ALIGN_WORD, ENTRY_BYTES, ENTRY_KEY, FAULT_CAPABILITY, FAULT_NOT_A_FUNCTION, FN_ELEMENT, FN_ENV,
    FnBuild, OBJ_ENTRIES, OBJ_LEN, RefusalNames, Rt, RtFunc, copy_loop, record_named_fault,
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
    /// The interned `","` that `a.join()` and `a.join(undefined)` separate
    /// with (ECMA-262 23.1.3.18 step 3). Zero when the plan does not carry
    /// [`Me::Join`]; nothing reads it then.
    pub(crate) comma: i32,
    /// The interned `"comparefn"` a `sort(x)` whose `x` is not a function
    /// names itself with (ECMA-262 23.1.3.30 step 1 is a TypeError); the
    /// host reads it back as the callee of `FAULT_NOT_A_FUNCTION`. Zero when
    /// the plan does not carry [`Me::SortWith`].
    pub(crate) comparefn: i32,
    /// Function index of `__str_cmp`, the code-unit comparison `<` uses;
    /// `sort`'s default order is that comparison over ToString forms.
    pub(crate) str_cmp: u32,
    /// The interned `"a fractional Math.pow exponent"` -- the one arm of
    /// `pow` this engine refuses rather than approximates (21.3.2.26 hands
    /// a finite non-integer exponent over a positive base to exp/log, which
    /// this engine does not carry). Zero when the plan does not want
    /// [`Me::MathPow`]; nothing reads it then.
    pub(crate) pow_exponent: i32,
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
    /// `Array.isArray(x)` -- ECMA-262 23.1.2.2, arriving as `x.__is_array()`
    /// from the parser's fold (the `Object.keys` route). One tag test; the
    /// receiver is *any* value, so the call site does not dispatch at all.
    IsArray,
    /// `a.concat(x)` -- ECMA-262 23.1.3.1 with one argument and no symbols:
    /// a new array of the receiver's elements followed by `x`'s if `x` is an
    /// array, or by `x` itself if it is not. The downstream `cu-macos-smoke`
    /// built its argv by hand for want of it.
    Concat,
    /// `a.concat(x, y)`: the two-argument form, [`Me::Concat`] twice. Its
    /// own variant because arity is part of what a call site denotes.
    Concat2,
    /// `a.join(sep)` -- ECMA-262 23.1.3.18. `undefined` and `null` elements
    /// are empty (step 7.c); every other element goes through ToString, so
    /// an Object or an Array element is the same named refusal `"" + o` is
    /// -- never a fabricated `[object Object]`.
    Join,
    /// `a.join()`: the separator defaults to `","`. Three instructions
    /// around [`Me::Join`].
    JoinDefault,
    /// `a.sort()` -- ECMA-262 23.1.3.30 with no comparator: String order
    /// of the elements' ToString forms, `undefined` last. Two hand-written
    /// merge sorts in the downstream corpus are what this retires.
    Sort,
    /// `a.sort(f)`: the comparator form. Its own variant for the arity.
    SortWith,
    /// `sort_core(a, cmp)`: the merge sort both forms share -- stable, in
    /// place, `n` elements of scratch. A helper.
    SortCore,
    /// `sort_less(x, y, cmp) -> i32`: 23.1.3.30's SortCompare reduced to
    /// "does `x` sort before `y`", which is the only question a merge asks.
    /// A helper.
    SortLess,
    /// `s.charCodeAt(i)` -- ECMA-262 22.1.3.3. The UTF-16 code unit at
    /// position `i`, found by the same code-unit walk `slice` does; a
    /// position inside a surrogate pair answers that half, which is a
    /// *Number* and needs no refusal. NaN past either end.
    CharCodeAt,
    /// `s.charAt(i)` -- 22.1.3.2: `slice(i, i + 1)`, so a position on the
    /// second half of a pair is the same named refusal a `slice` boundary
    /// there is. `""` past either end.
    CharAt,
    /// `s.substring(a, b)` -- 22.1.3.24: both positions clamped to
    /// `[0, length]`, then swapped if out of order. `slice` with those two
    /// rules in place of its negative-from-the-end rule.
    Substring,
    /// `s.substring(a)`: `b` is the length.
    SubstringFrom,
    /// `s[i]`, the computed read on a String receiver: the code unit at an
    /// integer index as a one-unit String, `undefined` past the end, and
    /// the ordinary property path for any other key. Never a call site;
    /// wanted by the scan for a program that writes a computed read whose
    /// key the text does not settle.
    StrIndex,
    /// ECMA-262 7.1.6 ToInt32 of a value, as a raw `i32`: the one
    /// conversion the six bitwise operators and `~` share, and the whole of
    /// what a program pays beyond the operator it writes. Never a call
    /// site; wanted as a helper.
    ToInt32,
    /// The bitwise and shift operators (13.10, 13.12) and `~` (13.5.6).
    /// Not methods -- an operator has no name to call it by -- but gated
    /// the way a method is: the scan wants the one the program writes, and
    /// a program that writes none carries none. `UShr` alone reads its
    /// result as unsigned (7.1.7 ToUint32 has the same bits).
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    BitNot,
    /// The `Math` functions (ECMA-262 21.3.2), folded by the parser to
    /// reserved method names (`Math.floor(x)` is `x.__math_floor()`), each
    /// gated per name like any method. `min`/`max` reach here only with
    /// two arguments -- the parser folds the other arities to literals or
    /// `+x` -- and IEEE `f64.min`/`f64.max` are exactly 21.3.2.25/24 over
    /// a pair: NaN propagates and `-0` sorts below `+0`.
    MathFloor,
    MathCeil,
    MathRound,
    MathTrunc,
    MathAbs,
    MathSqrt,
    MathSign,
    MathPow,
    MathMin,
    MathMax,
    /// `parseInt(s[, radix])` (ECMA-262 19.2.5), folded by the parser to
    /// `s.__parse_int_radix(radix)`: leading StrWhiteSpaceChar skipped,
    /// one sign, `0x`/`0X` with radix 16 or by default, digits of the
    /// radix accumulated until the first that is not one, NaN when none.
    ParseInt,
    /// `Number.isInteger(x)` (21.1.2.3): a type test, not a conversion.
    IsInteger,
    /// `Number.isNaN(x)` (21.1.2.4): NaN of the Number type only.
    IsNan,
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
    Me::IsArray,
    Me::Concat,
    Me::Concat2,
    Me::Join,
    Me::JoinDefault,
    Me::Sort,
    Me::SortWith,
    Me::SortCore,
    Me::SortLess,
    Me::CharCodeAt,
    Me::CharAt,
    Me::Substring,
    Me::SubstringFrom,
    Me::StrIndex,
    Me::ToInt32,
    Me::BitAnd,
    Me::BitOr,
    Me::BitXor,
    Me::Shl,
    Me::Shr,
    Me::UShr,
    Me::BitNot,
    Me::MathFloor,
    Me::MathCeil,
    Me::MathRound,
    Me::MathTrunc,
    Me::MathAbs,
    Me::MathSqrt,
    Me::MathSign,
    Me::MathPow,
    Me::MathMin,
    Me::MathMax,
    Me::ParseInt,
    Me::IsInteger,
    Me::IsNan,
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
            Me::IsArray => "__m_is_array",
            Me::Concat => "__m_concat",
            Me::Concat2 => "__m_concat2",
            Me::Join => "__m_join",
            Me::JoinDefault => "__m_join_default",
            Me::Sort => "__m_sort",
            Me::SortWith => "__m_sort_with",
            Me::SortCore => "__m_sort_core",
            Me::SortLess => "__m_sort_less",
            Me::CharCodeAt => "__m_char_code_at",
            Me::CharAt => "__m_char_at",
            Me::Substring => "__m_substring",
            Me::SubstringFrom => "__m_substring_from",
            Me::StrIndex => "__m_str_index",
            Me::ToInt32 => "__m_to_int32",
            Me::BitAnd => "__m_bit_and",
            Me::BitOr => "__m_bit_or",
            Me::BitXor => "__m_bit_xor",
            Me::Shl => "__m_shl",
            Me::Shr => "__m_shr",
            Me::UShr => "__m_ushr",
            Me::BitNot => "__m_bit_not",
            Me::MathFloor => "__m_math_floor",
            Me::MathCeil => "__m_math_ceil",
            Me::MathRound => "__m_math_round",
            Me::MathTrunc => "__m_math_trunc",
            Me::MathAbs => "__m_math_abs",
            Me::MathSqrt => "__m_math_sqrt",
            Me::MathSign => "__m_math_sign",
            Me::MathPow => "__m_math_pow",
            Me::MathMin => "__m_math_min",
            Me::MathMax => "__m_math_max",
            Me::ParseInt => "__m_parse_int",
            Me::IsInteger => "__m_is_integer",
            Me::IsNan => "__m_is_nan",
        }
    }

    /// The prefab behind one bitwise operator. `None` for every other
    /// operator, which the unconditional runtime answers.
    pub(crate) fn of_binary(op: ast::BinaryOp) -> Option<Self> {
        Some(match op {
            ast::BinaryOp::BitAnd => Me::BitAnd,
            ast::BinaryOp::BitOr => Me::BitOr,
            ast::BinaryOp::BitXor => Me::BitXor,
            ast::BinaryOp::Shl => Me::Shl,
            ast::BinaryOp::Shr => Me::Shr,
            ast::BinaryOp::UShr => Me::UShr,
            _ => return None,
        })
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
            ("__is_array", 0) => Some(Me::IsArray),
            ("concat", 1) => Some(Me::Concat),
            ("concat", 2) => Some(Me::Concat2),
            ("join", 1) => Some(Me::Join),
            ("join", 0) => Some(Me::JoinDefault),
            ("sort", 0) => Some(Me::Sort),
            ("sort", 1) => Some(Me::SortWith),
            ("charCodeAt", 1) => Some(Me::CharCodeAt),
            ("charAt", 1) => Some(Me::CharAt),
            ("substring", 2) => Some(Me::Substring),
            ("substring", 1) => Some(Me::SubstringFrom),
            ("__math_floor", 0) => Some(Me::MathFloor),
            ("__math_ceil", 0) => Some(Me::MathCeil),
            ("__math_round", 0) => Some(Me::MathRound),
            ("__math_trunc", 0) => Some(Me::MathTrunc),
            ("__math_abs", 0) => Some(Me::MathAbs),
            ("__math_sqrt", 0) => Some(Me::MathSqrt),
            ("__math_sign", 0) => Some(Me::MathSign),
            ("__math_pow", 1) => Some(Me::MathPow),
            ("__math_min", 1) => Some(Me::MathMin),
            ("__math_max", 1) => Some(Me::MathMax),
            ("__parse_int_radix", 1) => Some(Me::ParseInt),
            ("__is_integer", 0) => Some(Me::IsInteger),
            ("__is_nan", 0) => Some(Me::IsNan),
            _ => None,
        }
    }

    /// The tag a call site must see before it may take the fast path. Two
    /// methods, two receivers -- so the call site's type test is per method,
    /// not one shared test. Small, but it is the call site that carries it,
    /// which is the shape criterion ⑥ is collecting.
    ///
    /// The third receiver kind ([`Recv::Obj`]) arrived with `Object.keys`:
    /// the call site used to test "array, else string", which sent an object
    /// receiver down the property path and into the String refusal --
    /// `Object.keys({})` trapped on its first day for exactly that. The
    /// fourth ([`Recv::StrOrArr`]) arrived with array `indexOf` /
    /// `includes`: one name, two receivers, and the text cannot tell them
    /// apart, so the prefab dispatches on the tag itself and the call site
    /// admits either. The fifth ([`Recv::Any`]) is `Array.isArray`, whose
    /// whole job is the tag test.
    pub(crate) fn receiver(self) -> Recv {
        match self {
            Me::Push
            | Me::Pop
            | Me::MapBound
            | Me::Concat
            | Me::Concat2
            | Me::Join
            | Me::JoinDefault
            | Me::Sort
            | Me::SortWith => Recv::Arr,
            Me::ObjKeys => Recv::Obj,
            Me::IndexOf | Me::Includes => Recv::StrOrArr,
            Me::IsArray => Recv::Any,
            Me::MathFloor
            | Me::MathCeil
            | Me::MathRound
            | Me::MathTrunc
            | Me::MathAbs
            | Me::MathSqrt
            | Me::MathSign
            | Me::MathPow
            | Me::MathMin
            | Me::MathMax
            | Me::ParseInt
            | Me::IsInteger
            | Me::IsNan => Recv::Any,
            Me::Trim
            | Me::StartsWith
            | Me::EndsWith
            | Me::Split
            | Me::ToLowerCase
            | Me::Slice
            | Me::SliceFrom
            | Me::Replace
            | Me::ReplaceAll
            | Me::CharCodeAt
            | Me::CharAt
            | Me::Substring
            | Me::SubstringFrom => Recv::Str,
            Me::WsWidth
            | Me::Units
            | Me::Substr
            | Me::Decode
            | Me::Encode
            | Me::LowerCp
            | Me::SliceCore
            | Me::SortCore
            | Me::SortLess
            | Me::StrIndex
            | Me::ToInt32
            | Me::BitAnd
            | Me::BitOr
            | Me::BitXor
            | Me::Shl
            | Me::Shr
            | Me::UShr
            | Me::BitNot => unreachable!("a helper or an operator is never a call site"),
        }
    }

    /// Whether this method's body reaches into the array set, which the
    /// *array* gate controls. See [`Ctx::array_base`].
    pub(crate) fn needs_arrays(self) -> bool {
        matches!(
            self,
            Me::Push | Me::MapBound | Me::Split | Me::ObjKeys | Me::Concat
        )
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
            Me::IsArray | Me::Concat => Vec::new(),
            Me::Concat2 => vec![Me::Concat],
            // `Substr` for the empty answer `[].join()` gives: a fresh
            // zero-length record rather than a second interned "".
            Me::Join => vec![Me::Substr],
            Me::JoinDefault => vec![Me::Join, Me::Substr],
            Me::Sort | Me::SortWith => vec![Me::SortCore, Me::SortLess],
            Me::SortCore => vec![Me::SortLess],
            Me::SortLess => Vec::new(),
            Me::CharCodeAt => vec![Me::Decode],
            Me::CharAt | Me::Substring | Me::SubstringFrom | Me::StrIndex => {
                vec![Me::SliceCore, Me::Substr]
            }
            Me::ToInt32 => Vec::new(),
            Me::BitAnd | Me::BitOr | Me::BitXor | Me::Shl | Me::Shr | Me::UShr | Me::BitNot => {
                vec![Me::ToInt32]
            }
            Me::MathFloor
            | Me::MathCeil
            | Me::MathRound
            | Me::MathTrunc
            | Me::MathAbs
            | Me::MathSqrt
            | Me::MathSign
            | Me::MathPow
            | Me::MathMin
            | Me::MathMax => Vec::new(),
            Me::ParseInt => vec![Me::ToInt32, Me::WsWidth],
            Me::IsInteger | Me::IsNan => Vec::new(),
        }
    }
}

/// What a call site must see in the receiver before it calls the prefab
/// directly; anything else is the ordinary property read and indirect call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Recv {
    Str,
    Arr,
    Obj,
    /// A String or an Array: the prefab tests the tag again and takes the
    /// arm the receiver wants.
    StrOrArr,
    /// No test at all: the prefab is the answer for every value.
    Any,
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
        Me::Includes => (values(2), values(1), includes(ctx)),
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
        Me::IsArray => (values(1), values(1), is_array_prefab()),
        Me::Concat => (values(2), values(1), concat(ctx)),
        Me::Concat2 => (values(3), values(1), concat2(ctx)),
        Me::Join => (values(2), values(1), join(ctx)),
        Me::JoinDefault => (values(1), values(1), join_default(ctx)),
        Me::Sort => (values(1), values(1), sort(ctx, false)),
        Me::SortWith => (values(2), values(1), sort(ctx, true)),
        Me::SortCore => (values(2), vec![], sort_core(ctx)),
        Me::SortLess => (values(3), vec![i32_], sort_less(ctx)),
        Me::CharCodeAt => (values(2), values(1), char_code_at(ctx)),
        Me::CharAt => (values(2), values(1), char_at(ctx)),
        Me::Substring => (values(3), values(1), substring(ctx, true)),
        Me::SubstringFrom => (values(2), values(1), substring(ctx, false)),
        Me::StrIndex => (values(2), values(1), str_index(ctx)),
        Me::ToInt32 => (values(1), vec![i32_], to_int32(ctx)),
        Me::BitAnd => (values(2), values(1), bitwise(ctx, Bit::And)),
        Me::BitOr => (values(2), values(1), bitwise(ctx, Bit::Or)),
        Me::BitXor => (values(2), values(1), bitwise(ctx, Bit::Xor)),
        Me::Shl => (values(2), values(1), bitwise(ctx, Bit::Shl)),
        Me::Shr => (values(2), values(1), bitwise(ctx, Bit::Shr)),
        Me::UShr => (values(2), values(1), bitwise(ctx, Bit::UShr)),
        Me::BitNot => (values(1), values(1), bit_not(ctx)),
        Me::MathFloor => (values(1), values(1), math_simple(ctx, Ins::F64Floor)),
        Me::MathCeil => (values(1), values(1), math_simple(ctx, Ins::F64Ceil)),
        Me::MathRound => (values(1), values(1), math_round(ctx)),
        Me::MathTrunc => (values(1), values(1), math_simple(ctx, Ins::F64Trunc)),
        Me::MathAbs => (values(1), values(1), math_simple(ctx, Ins::F64Abs)),
        Me::MathSqrt => (values(1), values(1), math_simple(ctx, Ins::F64Sqrt)),
        Me::MathSign => (values(1), values(1), math_sign(ctx)),
        Me::MathPow => (values(2), values(1), math_pow(ctx)),
        Me::MathMin => (values(2), values(1), math_pair(ctx, Ins::F64Min)),
        Me::MathMax => (values(2), values(1), math_pair(ctx, Ins::F64Max)),
        Me::ParseInt => (values(2), values(1), parse_int(ctx)),
        Me::IsInteger => (values(1), values(1), is_integer()),
        Me::IsNan => (values(1), values(1), is_nan()),
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

    array_search(ctx, &mut f, Found::Index);
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
fn includes(ctx: &Ctx) -> FnBuild {
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

    array_search(ctx, &mut f, Found::Bool);
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
    let t = f.local(ValType::I32);
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
    // The position the walk is heading for: `from` until it is found,
    // `to` after. What `ascii_skip` must not step past.
    b.push(Ins::LocalGet(from));
    b.push(Ins::LocalSet(t));

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    // Since 2026-08-31: eight ASCII bytes at a time while the position is
    // further than that (`slice(0, 10)` never takes it; `s[900]` on a
    // 1 000-character line takes it 112 times).
    ascii_skip(b, h, i, hl, u, t);
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
    b.push(Ins::LocalGet(to));
    b.push(Ins::LocalSet(t));
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

/// Eight plain-ASCII bytes are eight code units: at the top of a
/// code-unit walk, while eight bytes remain, the next eight are all below
/// 0x80 (two words, or-ed, against `0x80808080` -- `__len`'s test) and
/// the position being looked for is at least eight units away, step over
/// them in one go. Emitted inside the walk's `loop`, so the `br` is to
/// depth 1 from inside the `if`. `t` holds the position the walk is
/// heading for.
fn ascii_skip(b: &mut Vec<Ins>, h: u32, i: u32, hl: u32, u: u32, t: u32) {
    // !(t < u + 8) && !(hl < i + 8)
    b.push(Ins::LocalGet(t));
    b.push(Ins::LocalGet(u));
    b.push(Ins::I32Const(8));
    b.push(Ins::I32Add);
    b.push(Ins::I32LtU);
    b.push(Ins::I32Eqz);
    b.push(Ins::LocalGet(hl));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(8));
    b.push(Ins::I32Add);
    b.push(Ins::I32LtU);
    b.push(Ins::I32Eqz);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load(0, 4));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load(0, 8));
    b.push(Ins::I32Or);
    b.push(Ins::I32Const(0x8080_8080u32 as i32));
    b.push(Ins::I32And);
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(8));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::LocalGet(u));
    b.push(Ins::I32Const(8));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(u));
    b.push(Ins::Br(2));
    b.push(Ins::End);
    b.push(Ins::End);
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

/// What an array search answers: `indexOf`'s position or `includes`'s yes.
#[derive(Clone, Copy, PartialEq)]
enum Found {
    Index,
    Bool,
}

/// The Array arm of `indexOf` (ECMA-262 23.1.3.17) and `includes`
/// (23.1.3.16), emitted at the top of the String body: when the receiver's
/// tag is `TAG_ARRAY` the elements are searched and the function returns
/// here; otherwise nothing happened and the String body follows.
///
/// One name, two receivers, and the text at the call site cannot tell them
/// apart -- `x.indexOf(y)` is a String method or an Array one depending on
/// what `x` holds at run time. So this is one prefab with a tag test rather
/// than two prefabs and a third call-site arm: the call site admits either
/// tag ([`Recv::StrOrArr`]) and the dispatch happens once, here. A program
/// that calls `indexOf` only on Strings pays this arm's bytes; that is the
/// price of the name being shared, and it is recorded in the pin ledger.
///
/// Elements are compared as 7.2.16 IsStrictlyEqual compares V1 pairs --
/// same tag, then a Number by value, a String by bytes (`__str_eq`), and
/// every other tag by payload -- inlined rather than through `__strict_eq`,
/// because the call was the whole price: 58 steps an element with it, 20
/// without, on a miss over Numbers. `indexOf` stops there (23.1.3.17 uses
/// IsStrictlyEqual, so `NaN` is never found); `includes` uses SameValueZero
/// (23.1.3.16), which differs in exactly one case -- `NaN` finds `NaN` -- and
/// that case is one extra test per element only when the needle *is* `NaN`.
///
/// `fromIndex` is not taken: neither call site in the demand corpus passes
/// one, and an arity that is not specialised is the ordinary property path.
fn array_search(ctx: &Ctx, f: &mut FnBuild, found: Found) {
    let a = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let elems = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let e = f.local(ValType::I32);
    let nan = f.local(ValType::I32);
    let hit = f.local(ValType::I32);
    let nt = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(0));
    b.push(Ins::I32Const(repr::TAG_ARRAY));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(1));
    b.push(Ins::I32WrapI64);
    b.push(Ins::LocalSet(a));
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalSet(n));
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalSet(elems));
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::LocalSet(nt));

    // SameValueZero's one difference: a NaN needle. Decided once, before
    // the loop, so a search for anything else pays one test in total.
    if found == Found::Bool {
        b.push(Ins::LocalGet(WIDTH));
        b.push(Ins::I32Const(TAG_NUMBER));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::LocalGet(WIDTH + 1));
        b.push(Ins::F64ReinterpretI64);
        b.push(Ins::LocalGet(WIDTH + 1));
        b.push(Ins::F64ReinterpretI64);
        b.push(Ins::F64Ne);
        b.push(Ins::LocalSet(nan));
        b.push(Ins::End);
    }

    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(elems));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(e));
    // IsStrictlyEqual, inline: nothing matches across tags, and within
    // one the payload's meaning decides. Three `if`s over the needle's tag
    // rather than an if/else chain -- this instruction set has no `else`
    // -- and the tag test first, so an element of another tag costs one
    // comparison.
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(hit));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::LocalGet(nt));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(nt));
    b.push(Ins::I32Const(TAG_NUMBER));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
    b.push(Ins::F64ReinterpretI64);
    b.push(Ins::LocalGet(WIDTH + 1));
    b.push(Ins::F64ReinterpretI64);
    b.push(Ins::F64Eq);
    b.push(Ins::LocalSet(hit));
    b.push(Ins::End);
    b.push(Ins::LocalGet(nt));
    b.push(Ins::I32Const(repr::TAG_STRING));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
    b.push(Ins::I32WrapI64);
    b.push(Ins::LocalGet(WIDTH + 1));
    b.push(Ins::I32WrapI64);
    b.push(ctx.rt(Rt::StrEq));
    b.push(Ins::LocalSet(hit));
    b.push(Ins::End);
    b.push(Ins::LocalGet(nt));
    b.push(Ins::I32Const(TAG_NUMBER));
    b.push(Ins::I32Ne);
    b.push(Ins::LocalGet(nt));
    b.push(Ins::I32Const(repr::TAG_STRING));
    b.push(Ins::I32Ne);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
    b.push(Ins::LocalGet(WIDTH + 1));
    b.push(Ins::I64Eq);
    b.push(Ins::LocalSet(hit));
    b.push(Ins::End);
    b.push(Ins::End);
    if found == Found::Bool {
        b.push(Ins::LocalGet(nan));
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::LocalGet(e));
        b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
        b.push(Ins::I32Const(TAG_NUMBER));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::LocalGet(e));
        b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
        b.push(Ins::F64ReinterpretI64);
        b.push(Ins::LocalGet(e));
        b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
        b.push(Ins::F64ReinterpretI64);
        b.push(Ins::F64Ne);
        b.push(Ins::LocalGet(hit));
        b.push(Ins::I32Or);
        b.push(Ins::LocalSet(hit));
        b.push(Ins::End);
        b.push(Ins::End);
    }
    b.push(Ins::LocalGet(hit));
    b.push(Ins::If(BlockType::Empty));
    match found {
        Found::Index => {
            let inner = vec![Ins::LocalGet(i), Ins::F64ConvertI32S];
            let mut boxed = Vec::new();
            box_number(&inner, &mut boxed);
            b.extend(boxed);
        }
        Found::Bool => const_bool(true, b),
    }
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    match found {
        Found::Index => number_const(b, -1),
        Found::Bool => const_bool(false, b),
    }
    b.push(Ins::Return);
    b.push(Ins::End);
}

/// `Array.isArray(x)` -- ECMA-262 23.1.2.2, as `x.__is_array()`.
///
/// The tag *is* the answer: this engine has exactly one array
/// representation and no proxies, so 7.2.2's other steps have nothing to
/// look at. Takes any value, which is why the call site does not test the
/// receiver first ([`Recv::Any`]).
fn is_array_prefab() -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let inner = vec![Ins::LocalGet(0), Ins::I32Const(repr::TAG_ARRAY), Ins::I32Eq];
    let mut out = Vec::new();
    box_bool(&inner, &mut out);
    f.body.extend(out);
    f
}

/// `a.concat(x)` -- ECMA-262 23.1.3.1 with one argument.
///
/// Step 5's IsConcatSpreadable is the tag test: an Array argument
/// contributes its elements, anything else contributes itself. The new
/// array is built at its final size, so nothing grows; the elements are
/// copied as V1 pairs, which is what "shallow" means here -- a nested
/// array or object is shared, not cloned, exactly as the spec's `Set`
/// shares the value.
fn concat(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let a = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let m = f.local(ValType::I32);
    let src = f.local(ValType::I32);
    let out = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let e = f.local(ValType::I32);
    let dst = f.local(ValType::I32);

    unbox_array(0, &mut f.body);
    f.body.push(Ins::LocalSet(a));

    let b = &mut f.body;
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalSet(n));
    // How many the argument contributes: its length when it is an array,
    // one otherwise. Decided once so the allocation is exact.
    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(m));
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::I32Const(repr::TAG_ARRAY));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(WIDTH + 1));
    b.push(Ins::I32WrapI64);
    b.push(Ins::LocalTee(src));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalSet(m));
    b.push(Ins::End);
    b.push(Ins::LocalGet(n));
    b.push(Ins::LocalGet(m));
    b.push(Ins::I32Add);
    b.push(ctx.arr(Ar::New));
    b.push(Ins::LocalSet(out));
    b.push(Ins::LocalGet(out));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalSet(dst));

    // The receiver's elements, then the argument's -- the same loop over
    // two vectors, written once and run twice. Stored straight into the
    // vector rather than through `__arr_push`: the capacity is exact, so
    // the grow test the push would run is dead, and the call was half the
    // price (53 -> 24 steps an element). The length is written once, at
    // the end.
    for (vector, count) in [(a, n), (src, m)] {
        if vector == src {
            b.push(Ins::LocalGet(WIDTH));
            b.push(Ins::I32Const(repr::TAG_ARRAY));
            b.push(Ins::I32Eq);
            b.push(Ins::If(BlockType::Empty));
        }
        b.push(Ins::I32Const(0));
        b.push(Ins::LocalSet(i));
        b.push(Ins::Block(BlockType::Empty));
        b.push(Ins::Loop(BlockType::Empty));
        b.push(Ins::LocalGet(i));
        b.push(Ins::LocalGet(count));
        b.push(Ins::I32GeU);
        b.push(Ins::BrIf(1));
        b.push(Ins::LocalGet(vector));
        b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
        b.push(Ins::LocalGet(i));
        b.push(Ins::I32Const(ELEM_BYTES));
        b.push(Ins::I32Mul);
        b.push(Ins::I32Add);
        b.push(Ins::LocalSet(e));
        b.push(Ins::LocalGet(dst));
        b.push(Ins::LocalGet(e));
        b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
        b.push(Ins::I32Store(ALIGN_WORD, ELEM_TAG));
        b.push(Ins::LocalGet(dst));
        b.push(Ins::LocalGet(e));
        b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
        b.push(Ins::I64Store(ALIGN_WORD, ELEM_PAYLOAD));
        b.push(Ins::LocalGet(dst));
        b.push(Ins::I32Const(ELEM_BYTES));
        b.push(Ins::I32Add);
        b.push(Ins::LocalSet(dst));
        b.push(Ins::LocalGet(i));
        b.push(Ins::I32Const(1));
        b.push(Ins::I32Add);
        b.push(Ins::LocalSet(i));
        b.push(Ins::Br(0));
        b.push(Ins::End);
        b.push(Ins::End);
        if vector == src {
            b.push(Ins::End);
        }
    }
    // A non-array argument is one element, itself.
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::I32Const(repr::TAG_ARRAY));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::I32Store(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalGet(WIDTH + 1));
    b.push(Ins::I64Store(ALIGN_WORD, ELEM_PAYLOAD));
    b.push(Ins::End);
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(n));
    b.push(Ins::LocalGet(m));
    b.push(Ins::I32Add);
    b.push(Ins::I32Store(ALIGN_WORD, ARR_LEN));

    let inner = vec![Ins::LocalGet(out)];
    let mut boxed = Vec::new();
    box_array(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `a.concat(x, y)`: `a.concat(x).concat(y)`, which is what 23.1.3.1's loop
/// over the argument list amounts to. The intermediate array is left to the
/// heap, as every intermediate is.
fn concat2(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(3 * WIDTH);
    let b = &mut f.body;
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(1));
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::LocalGet(WIDTH + 1));
    b.push(ctx.me(Me::Concat));
    b.push(Ins::LocalGet(2 * WIDTH));
    b.push(Ins::LocalGet(2 * WIDTH + 1));
    b.push(ctx.me(Me::Concat));
    f
}

/// Copy the `n`-byte body of the string record at `src` into the record at
/// `dst` from body offset `at`, and leave `at + n` in `at`. The copy is
/// `__str_concat`'s own word loop.
fn copy_body(b: &mut Vec<Ins>, dst: u32, at: u32, src: u32, n: u32, k: u32) {
    copy_loop(b, src, n, dst, Some(at), k);
    b.push(Ins::LocalGet(at));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(at));
}

/// `a.join(sep)` -- ECMA-262 23.1.3.18.
///
/// Two passes, so the answer is built once at its exact size rather than
/// by repeated concatenation (quadratic in bytes on a heap that never
/// frees). The first pass converts every element -- `undefined` and `null`
/// to nothing (step 7.c), everything else through `__to_string`, whose
/// refusal of an Object or an Array is the same named fault `"" + o`
/// raises -- and parks the record pointers in a scratch word vector while
/// summing their lengths; the second copies them out with the separator
/// between. The scratch vector is `4n` bytes and is left to the heap.
///
/// A separator that is not a String goes through ToString (step 3-4);
/// `undefined` is the one value that means "use a comma" rather than
/// "spell me", and [`Me::JoinDefault`] is how a call with no argument
/// reaches that.
fn join(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let a = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let elems = f.local(ValType::I32);
    let sep = f.local(ValType::I32);
    let sl = f.local(ValType::I32);
    let ptrs = f.local(ValType::I32);
    let total = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let e = f.local(ValType::I32);
    let piece = f.local(ValType::I32);
    let pl = f.local(ValType::I32);
    let out = f.local(ValType::I32);
    let at = f.local(ValType::I32);
    let k = f.local(ValType::I32);

    unbox_array(0, &mut f.body);
    f.body.push(Ins::LocalSet(a));

    let b = &mut f.body;
    // The separator: a comma for `undefined`, ToString otherwise.
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::I32Const(TAG_UNDEFINED));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(ctx.comma));
    b.push(Ins::LocalSet(sep));
    b.push(Ins::End);
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::I32Const(TAG_UNDEFINED));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(WIDTH));
    b.push(Ins::LocalGet(WIDTH + 1));
    b.push(ctx.rt(Rt::ToStr));
    b.push(Ins::LocalSet(sep));
    b.push(Ins::End);
    b.push(Ins::LocalGet(sep));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(sl));

    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalSet(n));
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalSet(elems));

    // Step 6: an empty array joins to the empty String.
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    let empty = vec![
        Ins::I32Const(0),
        Ins::I32Const(0),
        Ins::I32Const(0),
        ctx.me(Me::Substr),
    ];
    let mut boxed = Vec::new();
    box_string(&empty, &mut boxed);
    b.extend(boxed);
    b.push(Ins::Return);
    b.push(Ins::End);

    // Pass one: convert, park, and measure.
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Mul);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(ptrs));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalGet(sl));
    b.push(Ins::I32Mul);
    b.push(Ins::LocalSet(total));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(i));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(elems));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(e));
    // A zero pointer stands for "nothing": undefined and null (step 7.c).
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(piece));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::I32Const(TAG_UNDEFINED));
    b.push(Ins::I32Ne);
    b.push(Ins::LocalGet(e));
    b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::I32Const(TAG_NULL));
    b.push(Ins::I32Ne);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
    b.push(Ins::LocalGet(e));
    b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
    b.push(ctx.rt(Rt::ToStr));
    b.push(Ins::LocalTee(piece));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalGet(total));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(total));
    b.push(Ins::End);
    b.push(Ins::LocalGet(ptrs));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(piece));
    b.push(Ins::I32Store(ALIGN_WORD, 0));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    // Pass two: one allocation, then the bytes in order.
    b.push(Ins::LocalGet(total));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(out));
    b.push(Ins::LocalGet(out));
    b.push(Ins::LocalGet(total));
    b.push(Ins::I32Store(ALIGN_WORD, 0));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(at));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(i));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(i));
    b.push(Ins::If(BlockType::Empty));
    copy_body(b, out, at, sep, sl, k);
    b.push(Ins::End);
    b.push(Ins::LocalGet(ptrs));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Mul);
    b.push(Ins::I32Add);
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalTee(piece));
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(piece));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(pl));
    copy_body(b, out, at, piece, pl, k);
    b.push(Ins::End);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    let inner = vec![Ins::LocalGet(out)];
    let mut boxed = Vec::new();
    box_string(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `a.join()`: [`Me::Join`] with `undefined`, which it reads as the comma.
fn join_default(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let b = &mut f.body;
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(1));
    const_undefined(b);
    b.push(ctx.me(Me::Join));
    f
}

/// `a.sort()` and `a.sort(f)` -- ECMA-262 23.1.3.30.
///
/// Both are [`Me::SortCore`] on the receiver's record plus the answer, which
/// step 11 makes the receiver itself. A comparator that is not a function
/// is the TypeError of step 1, named `comparefn` for the host
/// (`FAULT_NOT_A_FUNCTION`); `undefined` is the default order.
fn sort(ctx: &Ctx, with: bool) -> FnBuild {
    let mut f = FnBuild::new(if with { 2 * WIDTH } else { WIDTH });
    let a = f.local(ValType::I32);
    unbox_array(0, &mut f.body);
    f.body.push(Ins::LocalSet(a));

    let b = &mut f.body;
    if with {
        b.push(Ins::LocalGet(WIDTH));
        b.push(Ins::I32Const(repr::TAG_FUNCTION));
        b.push(Ins::I32Ne);
        b.push(Ins::LocalGet(WIDTH));
        b.push(Ins::I32Const(TAG_UNDEFINED));
        b.push(Ins::I32Ne);
        b.push(Ins::I32And);
        b.push(Ins::If(BlockType::Empty));
        record_named_fault(ctx.comparefn, FAULT_NOT_A_FUNCTION, b);
        b.push(Ins::Unreachable);
        b.push(Ins::End);
    }
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(1));
    if with {
        b.push(Ins::LocalGet(WIDTH));
        b.push(Ins::LocalGet(WIDTH + 1));
    } else {
        const_undefined(b);
    }
    b.push(ctx.me(Me::SortCore));
    b.push(Ins::LocalGet(0));
    b.push(Ins::LocalGet(1));
    f
}

/// `sort_core(a, cmp)`: a bottom-up merge sort of the record's vector.
///
/// # Why a merge sort, and why this one
///
/// 23.1.3.30 step 9 asks for a stable sort, and the corpus that wanted this
/// method had already written one by hand twice -- `prune_target_incremental`
/// says why in its own comment: "a root can hold up to 100000 records, which
/// insertion sort would not finish in budget". A merge sort is `n log n`
/// comparisons and stable by construction (a tie takes the left run), and
/// bottom-up needs no recursion, which matters in an engine that bounds
/// call depth.
///
/// One scratch vector of `n` elements, allocated once and left to the heap.
/// Each pass merges runs of `width` from one vector into the other; the
/// vectors swap roles rather than copying back, and a final copy happens
/// only when the last pass left the answer in the scratch. `n < 2` does
/// nothing at all.
fn sort_core(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let a = f.local(ValType::I32);
    let n = f.local(ValType::I32);
    let src = f.local(ValType::I32);
    let dst = f.local(ValType::I32);
    let width = f.local(ValType::I32);
    let lo = f.local(ValType::I32);
    let mid = f.local(ValType::I32);
    let hi = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let j = f.local(ValType::I32);
    let k = f.local(ValType::I32);
    let take_right = f.local(ValType::I32);
    let from = f.local(ValType::I32);
    let swap = f.local(ValType::I32);
    let cmp = WIDTH;

    unbox_array(0, &mut f.body);
    f.body.push(Ins::LocalSet(a));

    let b = &mut f.body;
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_LEN));
    b.push(Ins::LocalSet(n));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(2));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::LocalSet(src));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32Const(ELEM_BYTES));
    b.push(Ins::I32Mul);
    b.push(ctx.rt(Rt::Alloc));
    b.push(Ins::LocalSet(dst));

    // One element, `src[from]` -> `dst[k]`, then `k += 1`. The three words
    // are copied as a word and a doubleword.
    let move_one = |b: &mut Vec<Ins>| {
        b.push(Ins::LocalGet(dst));
        b.push(Ins::LocalGet(k));
        b.push(Ins::I32Const(ELEM_BYTES));
        b.push(Ins::I32Mul);
        b.push(Ins::I32Add);
        b.push(Ins::LocalGet(src));
        b.push(Ins::LocalGet(from));
        b.push(Ins::I32Const(ELEM_BYTES));
        b.push(Ins::I32Mul);
        b.push(Ins::I32Add);
        b.push(Ins::LocalTee(from));
        b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
        b.push(Ins::I32Store(ALIGN_WORD, ELEM_TAG));
        b.push(Ins::LocalGet(dst));
        b.push(Ins::LocalGet(k));
        b.push(Ins::I32Const(ELEM_BYTES));
        b.push(Ins::I32Mul);
        b.push(Ins::I32Add);
        b.push(Ins::LocalGet(from));
        b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
        b.push(Ins::I64Store(ALIGN_WORD, ELEM_PAYLOAD));
        b.push(Ins::LocalGet(k));
        b.push(Ins::I32Const(1));
        b.push(Ins::I32Add);
        b.push(Ins::LocalSet(k));
    };
    // Push `src[index]` as a V1 pair.
    let element = |b: &mut Vec<Ins>, index: u32| {
        b.push(Ins::LocalGet(src));
        b.push(Ins::LocalGet(index));
        b.push(Ins::I32Const(ELEM_BYTES));
        b.push(Ins::I32Mul);
        b.push(Ins::I32Add);
        b.push(Ins::I32Load(ALIGN_WORD, ELEM_TAG));
        b.push(Ins::LocalGet(src));
        b.push(Ins::LocalGet(index));
        b.push(Ins::I32Const(ELEM_BYTES));
        b.push(Ins::I32Mul);
        b.push(Ins::I32Add);
        b.push(Ins::I64Load(ALIGN_WORD, ELEM_PAYLOAD));
    };

    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(width));
    // Passes: while width < n.
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(width));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(lo));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(k));
    // Runs: while lo < n.
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(lo));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    // mid = min(lo + width, n); hi = min(lo + 2 width, n).
    for (bound, times) in [(mid, 1), (hi, 2)] {
        b.push(Ins::LocalGet(lo));
        b.push(Ins::LocalGet(width));
        if times == 2 {
            b.push(Ins::I32Const(2));
            b.push(Ins::I32Mul);
        }
        b.push(Ins::I32Add);
        b.push(Ins::LocalSet(bound));
        b.push(Ins::LocalGet(bound));
        b.push(Ins::LocalGet(n));
        b.push(Ins::I32GeU);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::LocalGet(n));
        b.push(Ins::LocalSet(bound));
        b.push(Ins::End);
    }
    b.push(Ins::LocalGet(lo));
    b.push(Ins::LocalSet(i));
    b.push(Ins::LocalGet(mid));
    b.push(Ins::LocalSet(j));
    // Merge: while i < mid or j < hi.
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(mid));
    b.push(Ins::I32GeU);
    b.push(Ins::LocalGet(j));
    b.push(Ins::LocalGet(hi));
    b.push(Ins::I32GeU);
    b.push(Ins::I32And);
    b.push(Ins::BrIf(1));
    // The right element is taken only when the left run is spent, or when
    // it sorts strictly before the left head -- a tie takes the left, which
    // is what makes this stable.
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(mid));
    b.push(Ins::I32GeU);
    b.push(Ins::LocalSet(take_right));
    b.push(Ins::LocalGet(take_right));
    b.push(Ins::I32Eqz);
    b.push(Ins::LocalGet(j));
    b.push(Ins::LocalGet(hi));
    b.push(Ins::I32LtU);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    element(b, j);
    element(b, i);
    b.push(Ins::LocalGet(cmp));
    b.push(Ins::LocalGet(cmp + 1));
    b.push(ctx.me(Me::SortLess));
    b.push(Ins::LocalSet(take_right));
    b.push(Ins::End);
    b.push(Ins::LocalGet(take_right));
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(j));
    b.push(Ins::LocalSet(from));
    move_one(b);
    b.push(Ins::LocalGet(j));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(j));
    b.push(Ins::End);
    b.push(Ins::LocalGet(take_right));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalSet(from));
    move_one(b);
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::End);
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::LocalGet(hi));
    b.push(Ins::LocalSet(lo));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    // The pass wrote `dst`; swap the roles for the next.
    b.push(Ins::LocalGet(src));
    b.push(Ins::LocalSet(swap));
    b.push(Ins::LocalGet(dst));
    b.push(Ins::LocalSet(src));
    b.push(Ins::LocalGet(swap));
    b.push(Ins::LocalSet(dst));
    b.push(Ins::LocalGet(width));
    b.push(Ins::I32Const(2));
    b.push(Ins::I32Mul);
    b.push(Ins::LocalSet(width));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    // The answer is in `src`. When that is the scratch, copy it home:
    // the record's own vector must hold it, because every other reference
    // to this array reads that vector.
    b.push(Ins::LocalGet(src));
    b.push(Ins::LocalGet(a));
    b.push(Ins::I32Load(ALIGN_WORD, ARR_ELEMS));
    b.push(Ins::I32Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(k));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(from));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(k));
    b.push(Ins::LocalGet(n));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(k));
    b.push(Ins::LocalSet(from));
    move_one(b);
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::End);
    f
}

/// `sort_less(x, y, cmp) -> i32`: whether `x` sorts strictly before `y`.
///
/// 23.1.3.30's SortCompare, with the parts a merge does not ask about
/// left out. `undefined` sorts after everything and equal to itself
/// (steps 1-3, and there are no holes). With no comparator, both sides
/// go through ToString and the code-unit comparison `<` uses
/// (`__str_cmp`), so `[10, 9, 1].sort()` is `[1, 10, 9]` exactly as the
/// spec says and every engine does; an Object element is the same named
/// refusal `"" + o` is. With one, the comparator is called back the way
/// `map`'s is, its answer goes through ToNumber, and NaN is 0 (step 6):
/// "strictly before" is then `v < 0`.
fn sort_less(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(3 * WIDTH);
    let x = 0;
    let y = WIDTH;
    let cmp = 2 * WIDTH;
    let v = f.local(ValType::F64);
    let tag = f.local(ValType::I32);
    let payload = f.local(ValType::I64);
    let b = &mut f.body;

    // Step 1-3: undefined last.
    b.push(Ins::LocalGet(x));
    b.push(Ins::I32Const(TAG_UNDEFINED));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(0));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::LocalGet(y));
    b.push(Ins::I32Const(TAG_UNDEFINED));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(1));
    b.push(Ins::Return);
    b.push(Ins::End);

    // Step 4: the comparator, when there is one.
    if let Some((type_index, arity)) = ctx.uniform {
        b.push(Ins::LocalGet(cmp));
        b.push(Ins::I32Const(repr::TAG_FUNCTION));
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        // The same call `map_bound` makes: environment, arguments padded
        // to the uniform arity, element, `call_indirect`.
        b.push(Ins::LocalGet(cmp + 1));
        b.push(Ins::I32WrapI64);
        b.push(Ins::I32Load(ALIGN_WORD, FN_ENV));
        b.push(Ins::LocalGet(x));
        b.push(Ins::LocalGet(x + 1));
        if arity >= 2 {
            b.push(Ins::LocalGet(y));
            b.push(Ins::LocalGet(y + 1));
        }
        for _ in 2..arity {
            const_undefined(b);
        }
        b.push(Ins::LocalGet(cmp + 1));
        b.push(Ins::I32WrapI64);
        b.push(Ins::I32Load(ALIGN_WORD, FN_ELEMENT));
        b.push(Ins::CallIndirect(type_index, 0));
        b.push(Ins::LocalSet(payload));
        b.push(Ins::LocalSet(tag));
        b.push(Ins::LocalGet(tag));
        b.push(Ins::LocalGet(payload));
        b.push(ctx.rt(Rt::ToNumber));
        b.push(Ins::LocalSet(v));
        // NaN is 0, and 0 is not "before"; `v < 0` is false for NaN
        // already, so the test needs no NaN arm of its own.
        b.push(Ins::LocalGet(v));
        b.push(Ins::F64Const(0.0));
        b.push(Ins::F64Lt);
        b.push(Ins::Return);
        b.push(Ins::End);
    }

    // Step 5-9: String order of the ToString forms.
    b.push(Ins::LocalGet(x));
    b.push(Ins::LocalGet(x + 1));
    b.push(ctx.rt(Rt::ToStr));
    b.push(Ins::LocalGet(y));
    b.push(Ins::LocalGet(y + 1));
    b.push(ctx.rt(Rt::ToStr));
    b.push(Ins::Call(ctx.str_cmp));
    b.push(Ins::I32Const(-1));
    b.push(Ins::I32Eq);
    f
}

/// The integer a position argument denotes: ToIntegerOrInfinity (ECMA-262
/// 7.1.5) narrowed to `i32` -- NaN is 0, the value is clamped into the
/// `i32` range before the truncation so the truncation cannot trap, and
/// the clamp changes nothing a string could be long enough to notice. The
/// argument is a Number or it traps in `unbox_number`, which is this
/// engine's standing narrowing for method arguments (see `slice`).
fn integer_arg(b: &mut Vec<Ins>, arg: u32, r: u32, into: u32) {
    unbox_number(arg, b);
    b.push(Ins::LocalSet(r));
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
    b.push(Ins::LocalGet(r));
    b.push(Ins::I32TruncF64S);
    b.push(Ins::LocalSet(into));
}

/// A fresh empty String: `substr` of nothing.
fn empty_string(ctx: &Ctx, b: &mut Vec<Ins>) {
    let inner = vec![
        Ins::I32Const(0),
        Ins::I32Const(0),
        Ins::I32Const(0),
        ctx.me(Me::Substr),
    ];
    let mut boxed = Vec::new();
    box_string(&inner, &mut boxed);
    b.extend(boxed);
}

/// `s.charCodeAt(i)` -- ECMA-262 22.1.3.3.
///
/// The walk is `slice_core`'s -- a lead byte starts a code unit, a 4-byte
/// sequence is two -- with the character decoded when the position is
/// reached. A one-, two- or three-byte character *is* its code unit; a
/// four-byte one is a surrogate pair, and the position says which half:
/// `0xD800 + ((cp - 0x10000) >> 10)` or `0xDC00 + ((cp - 0x10000) & 0x3FF)`.
/// That half is a Number, and a Number is representable, which is why this
/// method needs no refusal where `charAt` on the same position does.
/// `NaN` for a position before the start or at or past the end (step 6).
fn char_code_at(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let h = f.local(ValType::I32);
    let idx = f.local(ValType::I32);
    let hl = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let u = f.local(ValType::I32);
    let byte = f.local(ValType::I32);
    let units = f.local(ValType::I32);
    let cp = f.local(ValType::I32);
    let w = f.local(ValType::I32);
    let r = f.local(ValType::F64);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(h));
    integer_arg(&mut f.body, WIDTH, r, idx);

    let b = &mut f.body;
    let nan = |b: &mut Vec<Ins>| {
        let mut boxed = Vec::new();
        box_number(&[Ins::F64Const(f64::NAN)], &mut boxed);
        b.extend(boxed);
        b.push(Ins::Return);
    };
    b.push(Ins::LocalGet(idx));
    b.push(Ins::I32Const(0));
    b.push(Ins::I32LtS);
    b.push(Ins::If(BlockType::Empty));
    nan(b);
    b.push(Ins::End);

    // The walk is `slice_core`'s: a byte at a time, a lead byte counts one
    // unit or two, and nothing is decoded until the position is reached
    // -- decoding every character on the way cost 76 steps a unit, the
    // count costs ~20.
    b.push(Ins::LocalGet(h));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::LocalSet(hl));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(hl));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    ascii_skip(b, h, i, hl, u, idx);
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
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(0xf0));
    b.push(Ins::I32GeU);
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(units));
    // Reached: the position is this character's first unit, or the
    // second of a pair's two.
    b.push(Ins::LocalGet(u));
    b.push(Ins::LocalGet(idx));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(units));
    b.push(Ins::I32Const(2));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(u));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(idx));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::I32Or);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.me(Me::Decode));
    b.push(Ins::LocalSet(w));
    b.push(Ins::LocalSet(cp));
    // One to three bytes: the character is its unit.
    b.push(Ins::LocalGet(units));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    let mut boxed = Vec::new();
    box_number(&[Ins::LocalGet(cp), Ins::F64ConvertI32S], &mut boxed);
    b.extend(boxed);
    b.push(Ins::Return);
    b.push(Ins::End);
    // Four: the high half at the first unit, the low at the second.
    b.push(Ins::LocalGet(u));
    b.push(Ins::LocalGet(idx));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    let mut boxed = Vec::new();
    box_number(
        &[
            Ins::LocalGet(cp),
            Ins::I32Const(0x10000),
            Ins::I32Sub,
            Ins::I32Const(10),
            Ins::I32ShrU,
            Ins::I32Const(0xd800),
            Ins::I32Add,
            Ins::F64ConvertI32S,
        ],
        &mut boxed,
    );
    b.extend(boxed);
    b.push(Ins::Return);
    b.push(Ins::End);
    let mut boxed = Vec::new();
    box_number(
        &[
            Ins::LocalGet(cp),
            Ins::I32Const(0x10000),
            Ins::I32Sub,
            Ins::I32Const(0x3ff),
            Ins::I32And,
            Ins::I32Const(0xdc00),
            Ins::I32Add,
            Ins::F64ConvertI32S,
        ],
        &mut boxed,
    );
    b.extend(boxed);
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::LocalGet(u));
    b.push(Ins::LocalGet(units));
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
    nan(b);
    f
}

/// `s.charAt(i)` -- ECMA-262 22.1.3.2: the one-unit String at `i`, or
/// `""` outside the string (step 5). `slice(i, i + 1)` through the shared
/// core, so a position on the second half of a surrogate pair is the core's
/// own named refusal -- there is no UTF-8 for that half, and this engine
/// does not fabricate one.
fn char_at(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let h = f.local(ValType::I32);
    let idx = f.local(ValType::I32);
    let r = f.local(ValType::F64);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(h));
    integer_arg(&mut f.body, WIDTH, r, idx);

    let b = &mut f.body;
    b.push(Ins::LocalGet(idx));
    b.push(Ins::I32Const(0));
    b.push(Ins::I32LtS);
    b.push(Ins::If(BlockType::Empty));
    empty_string(ctx, b);
    b.push(Ins::Return);
    b.push(Ins::End);
    // `idx + 1` must not wrap: the clamp above stops one short of i32::MAX.
    let inner = vec![
        Ins::LocalGet(h),
        Ins::LocalGet(idx),
        Ins::LocalGet(idx),
        Ins::I32Const(1),
        Ins::I32Add,
        ctx.me(Me::SliceCore),
    ];
    let mut boxed = Vec::new();
    box_string(&inner, &mut boxed);
    b.extend(boxed);
    f
}

/// `s.substring(a[, b])` -- ECMA-262 22.1.3.24.
///
/// Where `slice` counts a negative position from the end, `substring`
/// clamps it to 0 (step 5-6), and where `slice` answers `""` for
/// `from > to`, `substring` swaps them (step 7-8). Both clamp to the length,
/// which the core does on its own by treating a position past the end as
/// the end. The one-argument form takes `b` as the length (step 4).
fn substring(ctx: &Ctx, has_end: bool) -> FnBuild {
    let mut f = FnBuild::new(if has_end { 3 * WIDTH } else { 2 * WIDTH });
    let h = f.local(ValType::I32);
    let from = f.local(ValType::I32);
    let to = f.local(ValType::I32);
    let swap = f.local(ValType::I32);
    let r = f.local(ValType::F64);

    unbox_string(0, &mut f.body);
    f.body.push(Ins::LocalSet(h));
    integer_arg(&mut f.body, WIDTH, r, from);
    if has_end {
        integer_arg(&mut f.body, 2 * WIDTH, r, to);
    } else {
        f.body.push(Ins::I32Const(i32::MAX));
        f.body.push(Ins::LocalSet(to));
    }

    let b = &mut f.body;
    for edge in [from, to] {
        b.push(Ins::LocalGet(edge));
        b.push(Ins::I32Const(0));
        b.push(Ins::I32LtS);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::I32Const(0));
        b.push(Ins::LocalSet(edge));
        b.push(Ins::End);
    }
    b.push(Ins::LocalGet(to));
    b.push(Ins::LocalGet(from));
    b.push(Ins::I32LtS);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(from));
    b.push(Ins::LocalSet(swap));
    b.push(Ins::LocalGet(to));
    b.push(Ins::LocalSet(from));
    b.push(Ins::LocalGet(swap));
    b.push(Ins::LocalSet(to));
    b.push(Ins::End);
    let inner = vec![
        Ins::LocalGet(h),
        Ins::LocalGet(from),
        Ins::LocalGet(to),
        ctx.me(Me::SliceCore),
    ];
    let mut boxed = Vec::new();
    box_string(&inner, &mut boxed);
    b.extend(boxed);
    f
}

/// `__m_str_index(receiver, key) -> value`: a computed read whose receiver
/// turned out to be a String.
///
/// ECMA-262 10.4.3.5 (String exotic `[[GetOwnProperty]]`): an integer
/// index below the length is the one-unit String there (through the
/// shared core, so the second half of a pair is the core's named refusal),
/// an integer index at or past it is `undefined`, and every other key is
/// an ordinary property read -- `s["length"]` answers, `s.foo` names
/// itself as it did. The index test is inline rather than `__arr_index`'s
/// so a program with no arrays can carry this without the array set.
///
/// Reached two ways, for one price: in a program with arrays `__prop_get`
/// hands its String receivers here (the Array arm stays first, so `a[i]`
/// pays nothing); in one without, the emitter lowers every computed read
/// to this call, and the fall-through below *is* the lowering it replaced.
fn str_index(ctx: &Ctx) -> FnBuild {
    let recv = 0;
    let key = WIDTH;
    let mut f = FnBuild::new(2 * WIDTH);
    let d = f.local(ValType::F64);
    let idx = f.local(ValType::I32);
    let r = f.local(ValType::I32);
    let b = &mut f.body;

    b.push(Ins::LocalGet(recv));
    b.push(Ins::I32Const(repr::TAG_STRING));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(key));
    b.push(Ins::I32Const(TAG_NUMBER));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(key + 1));
    b.push(Ins::F64ReinterpretI64);
    b.push(Ins::LocalSet(d));
    // `0 <= d < 2^31 - 1`, which NaN fails, and then integral.
    b.push(Ins::LocalGet(d));
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Ge);
    b.push(Ins::LocalGet(d));
    b.push(Ins::F64Const(2_147_483_647.0));
    b.push(Ins::F64Lt);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(d));
    b.push(Ins::I32TruncF64S);
    b.push(Ins::LocalSet(idx));
    b.push(Ins::LocalGet(idx));
    b.push(Ins::F64ConvertI32S);
    b.push(Ins::LocalGet(d));
    b.push(Ins::F64Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(recv + 1));
    b.push(Ins::I32WrapI64);
    b.push(Ins::LocalGet(idx));
    b.push(Ins::LocalGet(idx));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(ctx.me(Me::SliceCore));
    b.push(Ins::LocalSet(r));
    // An index in range yields one unit; the core's empty answer is the
    // end of the string, and 10.4.3.5 says `undefined` there.
    b.push(Ins::LocalGet(r));
    b.push(Ins::I32Load(ALIGN_WORD, 0));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    const_undefined(b);
    b.push(Ins::Return);
    b.push(Ins::End);
    let mut boxed = Vec::new();
    box_string(&[Ins::LocalGet(r)], &mut boxed);
    b.extend(boxed);
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::End);
    // A Number that is not an index -- negative, fractional, NaN -- names
    // no property a String has (10.4.3.5 step 1.b falls through to an
    // ordinary object with no such key): `undefined`, as the spec says,
    // and not the missing-property refusal, which is for names a real
    // String *does* answer.
    const_undefined(b);
    b.push(Ins::Return);
    b.push(Ins::End);

    b.push(Ins::LocalGet(recv));
    b.push(Ins::LocalGet(recv + 1));
    b.push(Ins::LocalGet(key));
    b.push(Ins::LocalGet(key + 1));
    b.push(ctx.rt(Rt::ToStr));
    b.push(ctx.rt(Rt::ObjGet));
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

// ---- the bitwise operators ---------------------------------------------

/// `__m_to_int32(v) -> i32`: ECMA-262 7.1.6 over ToNumber.
///
/// NaN and the two infinities are 0 (steps 2-3); everything else is
/// truncated and reduced modulo 2^32 into the signed range (steps 4-5).
/// The reduction is `x - 2^32 * floor(x / 2^32)`, and every operation in it
/// is exact: the division and the multiplication scale by a power of two,
/// `floor` of a double is a double, and the final subtraction is of two
/// integers whose difference is below 2^32 -- Sterbenz for `|x| >= 2^33`,
/// and plain representability below that. The common case, `|x| < 2^31`,
/// takes none of that road: one compare and `i32.trunc_f64_s`, which cannot
/// trap there.
fn to_int32(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let x = f.local(ValType::F64);
    let b = &mut f.body;
    load_local(0, b);
    b.push(ctx.rt(Rt::ToNumber));
    b.push(Ins::LocalTee(x));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(0));
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Trunc);
    b.push(Ins::LocalTee(x));
    b.push(Ins::F64Abs);
    b.push(Ins::F64Const(2_147_483_648.0));
    b.push(Ins::F64Lt);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(x));
    b.push(Ins::I32TruncF64S);
    b.push(Ins::Return);
    b.push(Ins::End);
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Abs);
    b.push(Ins::F64Const(f64::INFINITY));
    b.push(Ins::F64Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(0));
    b.push(Ins::Return);
    b.push(Ins::End);
    // x = x - 2^32 * floor(x / 2^32), in [0, 2^32)
    b.push(Ins::LocalGet(x));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Const(4_294_967_296.0));
    b.push(Ins::F64Div);
    b.push(Ins::F64Floor);
    b.push(Ins::F64Const(4_294_967_296.0));
    b.push(Ins::F64Mul);
    b.push(Ins::F64Sub);
    b.push(Ins::LocalTee(x));
    b.push(Ins::F64Const(2_147_483_648.0));
    b.push(Ins::F64Ge);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Const(4_294_967_296.0));
    b.push(Ins::F64Sub);
    b.push(Ins::LocalSet(x));
    b.push(Ins::End);
    b.push(Ins::LocalGet(x));
    b.push(Ins::I32TruncF64S);
    f
}

#[derive(Clone, Copy)]
enum Bit {
    And,
    Or,
    Xor,
    Shl,
    Shr,
    UShr,
}

/// One of the six binary operators: ToInt32 of each side in order, the
/// shift count masked to five bits (13.12.1 step 6), the wasm instruction,
/// and the result read back as a Number -- signed for five of them,
/// unsigned for `>>>` (13.12.3: `ToUint32` of the left operand has the same
/// bits, and the shifted-in zeros make the result non-negative).
fn bitwise(ctx: &Ctx, which: Bit) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let mut inner = Vec::new();
    load_local(0, &mut inner);
    inner.push(ctx.me(Me::ToInt32));
    load_local(WIDTH, &mut inner);
    inner.push(ctx.me(Me::ToInt32));
    if matches!(which, Bit::Shl | Bit::Shr | Bit::UShr) {
        inner.push(Ins::I32Const(31));
        inner.push(Ins::I32And);
    }
    inner.push(match which {
        Bit::And => Ins::I32And,
        Bit::Or => Ins::I32Or,
        Bit::Xor => Ins::I32Xor,
        Bit::Shl => Ins::I32Shl,
        Bit::Shr => Ins::I32ShrS,
        Bit::UShr => Ins::I32ShrU,
    });
    inner.push(match which {
        Bit::UShr => Ins::F64ConvertI32U,
        _ => Ins::F64ConvertI32S,
    });
    let mut boxed = Vec::new();
    box_number(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `~x` (13.5.6): ToInt32, every bit flipped, read back signed.
fn bit_not(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let mut inner = Vec::new();
    load_local(0, &mut inner);
    inner.push(ctx.me(Me::ToInt32));
    inner.push(Ins::I32Const(-1));
    inner.push(Ins::I32Xor);
    inner.push(Ins::F64ConvertI32S);
    let mut boxed = Vec::new();
    box_number(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

// ---- the Math functions -------------------------------------------------

/// `floor` / `ceil` / `trunc` / `abs` / `sqrt` (21.3.2.16 / .10 / .35 /
/// .1 / .32): ToNumber, then the wasm instruction that *is* the spec's
/// operation -- IEEE `sqrt` of a negative is NaN and of `-0` is `-0`,
/// `floor`/`ceil`/`trunc` keep the zero's sign, all exactly as 21.3.2
/// writes them.
fn math_simple(ctx: &Ctx, op: Ins) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let mut inner = Vec::new();
    load_local(0, &mut inner);
    inner.push(ctx.rt(Rt::ToNumber));
    inner.push(op);
    let mut boxed = Vec::new();
    box_number(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `min` / `max` over exactly two Numbers (21.3.2.25 / .24): the wasm
/// instruction is the 2019 IEEE minimum/maximum, which is the spec's --
/// NaN wins over anything and `-0` is smaller than `+0`.
fn math_pair(ctx: &Ctx, op: Ins) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let mut inner = Vec::new();
    load_local(0, &mut inner);
    inner.push(ctx.rt(Rt::ToNumber));
    load_local(WIDTH, &mut inner);
    inner.push(ctx.rt(Rt::ToNumber));
    inner.push(op);
    let mut boxed = Vec::new();
    box_number(&inner, &mut boxed);
    f.body.extend(boxed);
    f
}

/// `Math.round` (21.3.2.28): ties toward +∞, which is `floor` plus one
/// exactly when the fraction reaches one half. `x - floor(x)` is exact
/// for every finite double (Sterbenz where the two share an exponent,
/// plain representability below one), so the half test cannot be off by
/// a rounding. An integer, an infinity and NaN come back unchanged
/// (`inf - inf` is NaN and NaN passes no test), and a result of zero
/// takes the argument's sign: `round(-0.3)` is `-0` (step 2).
fn math_round(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let x = f.local(ValType::F64);
    let r = f.local(ValType::F64);
    let b = &mut f.body;
    load_local(0, b);
    b.push(ctx.rt(Rt::ToNumber));
    b.push(Ins::LocalTee(x));
    b.push(Ins::F64Floor);
    b.push(Ins::LocalSet(r));
    // r + 1 when the fraction is at or past one half.
    b.push(Ins::LocalGet(x));
    b.push(Ins::LocalGet(r));
    b.push(Ins::F64Sub);
    b.push(Ins::F64Const(0.5));
    b.push(Ins::F64Ge);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(r));
    b.push(Ins::F64Const(1.0));
    b.push(Ins::F64Add);
    b.push(Ins::LocalSet(r));
    b.push(Ins::End);
    // A zero keeps the argument's sign.
    b.push(Ins::LocalGet(r));
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(r));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Copysign);
    b.push(Ins::LocalSet(r));
    b.push(Ins::End);
    let mut boxed = Vec::new();
    box_number(&[Ins::LocalGet(r)], &mut boxed);
    b.extend(boxed);
    f
}

/// `Math.sign` (21.3.2.30): NaN and the zeros come back unchanged, and
/// everything else is one with the argument's sign.
fn math_sign(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let x = f.local(ValType::F64);
    let b = &mut f.body;
    load_local(0, b);
    b.push(ctx.rt(Rt::ToNumber));
    b.push(Ins::LocalSet(x));
    b.push(Ins::LocalGet(x));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Ne);
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Eq);
    b.push(Ins::I32Or);
    b.push(Ins::If(BlockType::Empty));
    let mut boxed = Vec::new();
    box_number(&[Ins::LocalGet(x)], &mut boxed);
    b.extend(boxed);
    b.push(Ins::Return);
    b.push(Ins::End);
    let mut boxed = Vec::new();
    box_number(
        &[Ins::F64Const(1.0), Ins::LocalGet(x), Ins::F64Copysign],
        &mut boxed,
    );
    b.extend(boxed);
    f
}

/// `Math.pow` (21.3.2.26 over 6.1.6.1.3, Number::exponentiate).
///
/// The special cases are the spec's, in its order: an exponent of ±0 is 1
/// whatever the base (even NaN); a NaN anywhere else is NaN; an infinite
/// exponent asks only where |base| stands against one, and |base| = 1
/// exactly is NaN. An *integer* exponent -- every use the downstream
/// count found -- is exponentiation by squaring on the double, with a
/// negative exponent as one over the positive's answer; infinities and
/// signed zeros ride the multiplications and divisions to exactly the
/// spec's table (`(-0) ** -1` is `-Infinity` because `1 / -0` is).
///
/// A finite non-integer exponent over a positive base is the one arm
/// 6.1.6.1.3 leaves to exp/log, which this engine does not carry: it is
/// refused **by name** (`FAULT_CAPABILITY`, `a fractional Math.pow
/// exponent`) rather than approximated silently. Over a negative base it
/// is NaN (step 12), and over ±0 / ±∞ the spec's own table still answers.
fn math_pow(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let x = f.local(ValType::F64);
    let e = f.local(ValType::F64);
    let m = f.local(ValType::F64);
    let r = f.local(ValType::F64);
    let b = &mut f.body;
    load_local(0, b);
    b.push(ctx.rt(Rt::ToNumber));
    b.push(Ins::LocalSet(x));
    load_local(WIDTH, b);
    b.push(ctx.rt(Rt::ToNumber));
    b.push(Ins::LocalSet(e));
    let answer = |b: &mut Vec<Ins>, ins: &[Ins]| {
        let mut boxed = Vec::new();
        box_number(ins, &mut boxed);
        b.extend(boxed);
        b.push(Ins::Return);
    };
    // exponent ±0 -> 1, even for a NaN base (6.1.6.1.3 step 1).
    b.push(Ins::LocalGet(e));
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Eq);
    b.push(Ins::If(BlockType::Empty));
    answer(b, &[Ins::F64Const(1.0)]);
    b.push(Ins::End);
    // NaN anywhere else is NaN (steps 2-3).
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(e));
    b.push(Ins::F64Ne);
    b.push(Ins::LocalGet(x));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Ne);
    b.push(Ins::I32Or);
    b.push(Ins::If(BlockType::Empty));
    answer(b, &[Ins::F64Const(f64::NAN)]);
    b.push(Ins::End);
    // An infinite exponent (step 6): |base| = 1 is NaN, and otherwise the
    // answer is +∞ exactly when |base| > 1 agrees with e > 0.
    b.push(Ins::LocalGet(e));
    b.push(Ins::F64Abs);
    b.push(Ins::F64Const(f64::INFINITY));
    b.push(Ins::F64Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Abs);
    b.push(Ins::F64Const(1.0));
    b.push(Ins::F64Eq);
    b.push(Ins::If(BlockType::Empty));
    answer(b, &[Ins::F64Const(f64::NAN)]);
    b.push(Ins::End);
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Abs);
    b.push(Ins::F64Const(1.0));
    b.push(Ins::F64Gt);
    b.push(Ins::LocalGet(e));
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Gt);
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    answer(b, &[Ins::F64Const(f64::INFINITY)]);
    b.push(Ins::End);
    answer(b, &[Ins::F64Const(0.0)]);
    b.push(Ins::End);
    // A finite non-integer exponent: NaN over a negative base (step 12),
    // the spec's zero/infinity table over ±0 and ±∞ (their rows have no
    // odd-integer branch left when the exponent is not an integer), and a
    // named refusal over a positive finite base.
    b.push(Ins::LocalGet(e));
    b.push(Ins::LocalGet(e));
    b.push(Ins::F64Trunc);
    b.push(Ins::F64Ne);
    b.push(Ins::If(BlockType::Empty));
    {
        // (the outer `b` is still borrowed here)
        b.push(Ins::LocalGet(x));
        b.push(Ins::F64Const(0.0));
        b.push(Ins::F64Lt);
        b.push(Ins::LocalGet(x));
        b.push(Ins::F64Abs);
        b.push(Ins::F64Const(f64::INFINITY));
        b.push(Ins::F64Ne);
        b.push(Ins::I32And);
        b.push(Ins::If(BlockType::Empty));
        answer(b, &[Ins::F64Const(f64::NAN)]);
        b.push(Ins::End);
        // ±0 and ±∞ bases: |x| > 1 (that is, ∞) agreeing with e > 0 is ∞.
        b.push(Ins::LocalGet(x));
        b.push(Ins::F64Const(0.0));
        b.push(Ins::F64Eq);
        b.push(Ins::LocalGet(x));
        b.push(Ins::F64Abs);
        b.push(Ins::F64Const(f64::INFINITY));
        b.push(Ins::F64Eq);
        b.push(Ins::I32Or);
        b.push(Ins::If(BlockType::Empty));
        b.push(Ins::LocalGet(x));
        b.push(Ins::F64Abs);
        b.push(Ins::F64Const(1.0));
        b.push(Ins::F64Gt);
        b.push(Ins::LocalGet(e));
        b.push(Ins::F64Const(0.0));
        b.push(Ins::F64Gt);
        b.push(Ins::I32Eq);
        b.push(Ins::If(BlockType::Empty));
        answer(b, &[Ins::F64Const(f64::INFINITY)]);
        b.push(Ins::End);
        answer(b, &[Ins::F64Const(0.0)]);
        b.push(Ins::End);
        record_named_fault(ctx.pow_exponent, FAULT_CAPABILITY, b);
        b.push(Ins::Unreachable);
    }
    b.push(Ins::End);
    // An integer exponent: squaring, negative as one over the positive.
    b.push(Ins::LocalGet(e));
    b.push(Ins::F64Abs);
    b.push(Ins::LocalSet(m));
    b.push(Ins::F64Const(1.0));
    b.push(Ins::LocalSet(r));
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(m));
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Eq);
    b.push(Ins::BrIf(1));
    // Odd: the low bit of m, as m - 2 * trunc(m / 2).
    b.push(Ins::LocalGet(m));
    b.push(Ins::LocalGet(m));
    b.push(Ins::F64Const(2.0));
    b.push(Ins::F64Div);
    b.push(Ins::F64Trunc);
    b.push(Ins::LocalTee(m));
    b.push(Ins::F64Const(2.0));
    b.push(Ins::F64Mul);
    b.push(Ins::F64Sub);
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Ne);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(r));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Mul);
    b.push(Ins::LocalSet(r));
    b.push(Ins::End);
    b.push(Ins::LocalGet(x));
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Mul);
    b.push(Ins::LocalSet(x));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::LocalGet(e));
    b.push(Ins::F64Const(0.0));
    b.push(Ins::F64Lt);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::F64Const(1.0));
    b.push(Ins::LocalGet(r));
    b.push(Ins::F64Div);
    b.push(Ins::LocalSet(r));
    b.push(Ins::End);
    let mut boxed = Vec::new();
    box_number(&[Ins::LocalGet(r)], &mut boxed);
    b.extend(boxed);
    f
}

// ---- parseInt and the Number type tests ---------------------------------

/// `parseInt(s[, radix])` -- ECMA-262 19.2.5, whole.
///
/// A receiver that is not a String goes through ToString first (step 1),
/// which answers for Numbers and Booleans and refuses an Object by name,
/// as ToString everywhere does. Leading whitespace is `Me::WsWidth`'s set
/// (StrWhiteSpaceChar, the same table `trim` walks), the radix is
/// `Me::ToInt32` of the argument (`undefined` is 0, which means 10 with
/// the `0x` prefix live), and the digits accumulate as `v * R + d` on the
/// double -- exact to 2^53, and past it correctly rounded per step rather
/// than over the whole numeral, which is the one place this can sit an
/// ulp from a host's `parseInt`. The downstream demand parses config
/// integers; none is near 2^53.
fn parse_int(ctx: &Ctx) -> FnBuild {
    let mut f = FnBuild::new(2 * WIDTH);
    let h = f.local(ValType::I32);
    let len = f.local(ValType::I32);
    let i = f.local(ValType::I32);
    let sign = f.local(ValType::F64);
    let radix = f.local(ValType::I32);
    let strip = f.local(ValType::I32);
    let v = f.local(ValType::F64);
    let any = f.local(ValType::I32);
    let byte = f.local(ValType::I32);
    let d = f.local(ValType::I32);
    let ok = f.local(ValType::I32);
    let w = f.local(ValType::I32);

    let b = &mut f.body;
    let nan = |b: &mut Vec<Ins>| {
        let mut boxed = Vec::new();
        box_number(&[Ins::F64Const(f64::NAN)], &mut boxed);
        b.extend(boxed);
        b.push(Ins::Return);
    };
    // Step 1: ToString of anything that is not one already.
    repr::is_string(0, b);
    b.push(Ins::If(BlockType::Empty));
    unbox_string(0, b);
    b.push(Ins::LocalSet(h));
    b.push(Ins::End);
    repr::is_string(0, b);
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    load_local(0, b);
    b.push(ctx.rt(Rt::ToStr));
    b.push(Ins::LocalSet(h));
    b.push(Ins::End);
    b.push(Ins::LocalGet(h));
    b.push(Ins::I32Load(2, 0));
    b.push(Ins::LocalSet(len));

    // Steps 3-4: leading StrWhiteSpaceChar.
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(len));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Const(4));
    b.push(Ins::I32Add);
    b.push(ctx.me(Me::WsWidth));
    b.push(Ins::LocalTee(w));
    b.push(Ins::I32Eqz);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(w));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    // Steps 5-7: one sign.
    b.push(Ins::F64Const(1.0));
    b.push(Ins::LocalSet(sign));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(len));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::LocalSet(byte));
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(43));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::End);
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(45));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::F64Const(-1.0));
    b.push(Ins::LocalSet(sign));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::End);
    b.push(Ins::End);

    // Steps 8-10: the radix, 0 meaning "10, prefix live"; 16 keeps the
    // prefix live too, and anything outside 2..=36 is NaN.
    load_local(WIDTH, b);
    b.push(ctx.me(Me::ToInt32));
    b.push(Ins::LocalSet(radix));
    b.push(Ins::LocalGet(radix));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(10));
    b.push(Ins::LocalSet(radix));
    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(strip));
    b.push(Ins::End);
    b.push(Ins::LocalGet(radix));
    b.push(Ins::I32Const(2));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(35));
    b.push(Ins::I32GeU);
    b.push(Ins::If(BlockType::Empty));
    nan(b);
    b.push(Ins::End);
    b.push(Ins::LocalGet(radix));
    b.push(Ins::I32Const(16));
    b.push(Ins::I32Eq);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(strip));
    b.push(Ins::End);
    // Step 10's `0x` / `0X`.
    b.push(Ins::LocalGet(strip));
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalGet(len));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::I32Const(48));
    b.push(Ins::I32Eq);
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 5));
    b.push(Ins::I32Const(32));
    b.push(Ins::I32Or);
    b.push(Ins::I32Const(120));
    b.push(Ins::I32Eq);
    b.push(Ins::I32And);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::I32Const(16));
    b.push(Ins::LocalSet(radix));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(2));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::End);

    // Steps 11-13: the digits, stopping at the first that is not one.
    b.push(Ins::Block(BlockType::Empty));
    b.push(Ins::Loop(BlockType::Empty));
    b.push(Ins::LocalGet(i));
    b.push(Ins::LocalGet(len));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(h));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Add);
    b.push(Ins::I32Load8U(0, 4));
    b.push(Ins::LocalSet(byte));
    b.push(Ins::I32Const(0));
    b.push(Ins::LocalSet(ok));
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(48));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(10));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(48));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalSet(d));
    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(ok));
    b.push(Ins::End);
    b.push(Ins::LocalGet(ok));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(32));
    b.push(Ins::I32Or);
    b.push(Ins::I32Const(97));
    b.push(Ins::I32Sub);
    b.push(Ins::I32Const(26));
    b.push(Ins::I32LtU);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(byte));
    b.push(Ins::I32Const(32));
    b.push(Ins::I32Or);
    b.push(Ins::I32Const(87));
    b.push(Ins::I32Sub);
    b.push(Ins::LocalSet(d));
    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(ok));
    b.push(Ins::End);
    b.push(Ins::End);
    b.push(Ins::LocalGet(ok));
    b.push(Ins::I32Eqz);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(d));
    b.push(Ins::LocalGet(radix));
    b.push(Ins::I32GeU);
    b.push(Ins::BrIf(1));
    b.push(Ins::LocalGet(v));
    b.push(Ins::LocalGet(radix));
    b.push(Ins::F64ConvertI32S);
    b.push(Ins::F64Mul);
    b.push(Ins::LocalGet(d));
    b.push(Ins::F64ConvertI32S);
    b.push(Ins::F64Add);
    b.push(Ins::LocalSet(v));
    b.push(Ins::I32Const(1));
    b.push(Ins::LocalSet(any));
    b.push(Ins::LocalGet(i));
    b.push(Ins::I32Const(1));
    b.push(Ins::I32Add);
    b.push(Ins::LocalSet(i));
    b.push(Ins::Br(0));
    b.push(Ins::End);
    b.push(Ins::End);

    b.push(Ins::LocalGet(any));
    b.push(Ins::I32Eqz);
    b.push(Ins::If(BlockType::Empty));
    nan(b);
    b.push(Ins::End);
    let mut boxed = Vec::new();
    box_number(
        &[Ins::LocalGet(sign), Ins::LocalGet(v), Ins::F64Mul],
        &mut boxed,
    );
    b.extend(boxed);
    f
}

/// `Number.isInteger(x)` -- 21.1.2.3: false for anything that is not a
/// Number (no conversion), and for NaN and the infinities; the test is
/// `trunc(x) == x`.
fn is_integer() -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let r = f.local(ValType::I32);
    let x = f.local(ValType::F64);
    let b = &mut f.body;
    repr::is_number(0, b);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(1));
    b.push(Ins::F64ReinterpretI64);
    b.push(Ins::LocalTee(x));
    b.push(Ins::F64Trunc);
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Eq);
    b.push(Ins::LocalGet(x));
    b.push(Ins::F64Abs);
    b.push(Ins::F64Const(f64::INFINITY));
    b.push(Ins::F64Ne);
    b.push(Ins::I32And);
    b.push(Ins::LocalSet(r));
    b.push(Ins::End);
    let mut boxed = Vec::new();
    box_bool(&[Ins::LocalGet(r)], &mut boxed);
    b.extend(boxed);
    f
}

/// `Number.isNaN(x)` -- 21.1.2.4: NaN of the Number type only, so
/// `Number.isNaN("abc")` is false where a converting `isNaN` would lie.
fn is_nan() -> FnBuild {
    let mut f = FnBuild::new(WIDTH);
    let r = f.local(ValType::I32);
    let b = &mut f.body;
    repr::is_number(0, b);
    b.push(Ins::If(BlockType::Empty));
    b.push(Ins::LocalGet(1));
    b.push(Ins::F64ReinterpretI64);
    b.push(Ins::LocalGet(1));
    b.push(Ins::F64ReinterpretI64);
    b.push(Ins::F64Ne);
    b.push(Ins::LocalSet(r));
    b.push(Ins::End);
    let mut boxed = Vec::new();
    box_bool(&[Ins::LocalGet(r)], &mut boxed);
    b.extend(boxed);
    f
}
