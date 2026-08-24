//! What to compile against: the one thing a caller of this compiler chooses.
//!
//! A module of its own, and not three types in `lib.rs`, for a mechanical
//! reason worth stating: `src/parse.rs` reads [`Names`] and [`Options`] from
//! the crate root, and `tests/parse_m1.rs` compiles `src/parse.rs` under
//! `#[path]`, where the crate root is the test file. That test used to carry a
//! hand-copied twin of both types with a comment saying the two must stay
//! identical. One file both can name is the version of that arrangement that
//! cannot drift.
//!
//! # The one question
//!
//! What does a bare name in the source mean? The language itself has no
//! bindings, so under the default the answer is a capability diagnostic. The
//! [`eval_wasm`](tinyvm::eval_wasm) skin has exactly one binding table, so
//! there a name is a zero-argument import. And an embedder with a real host
//! door has a table of its own, which is [`Names::Declared`].

/// One raw wasm parameter position of a declared host function, and what
/// JavaScript value fills it.
///
/// The unwrapping is the *compiler's* job, which is the load-bearing half of
/// this design: the host door stays raw and language-neutral, so a
/// hand-written `.wasm` guest and a compiled `.qjs` guest reach the same host
/// through the same import table. A door that spoke this engine's two-word
/// value representation would break every hand-written guest and would leak
/// one language's value shape into a boundary meant to serve any guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostParam {
    /// A JavaScript String, unwrapped to **two** raw `i32` parameters: the
    /// address of its UTF-8 bytes in the guest's linear memory, then their
    /// byte length. The bytes themselves, not the `[len][bytes]` record the
    /// engine keeps them in.
    ///
    /// The address is only valid for the duration of the call: the guest's
    /// heap is a bump allocator the next call may move past.
    StrPtrLen,
    /// A JavaScript Number, unwrapped to one raw `i32`.
    ///
    /// The Number has to *be* an `i32`: a fractional value, a NaN, an infinity
    /// or anything outside the signed 32-bit range traps rather than being
    /// rounded, because a host that receives `3` where the script wrote `3.7`
    /// cannot tell the difference from one that received `3`.
    I32,
    /// A JavaScript Number, unwrapped to one raw `f64`. Lossless: a Number is
    /// a binary64 already.
    F64,
}

impl HostParam {
    /// The ECMA-262 language type a value must have to fill this, named as it
    /// reads inside a diagnostic.
    pub(crate) fn wants(self) -> &'static str {
        match self {
            Self::StrPtrLen => "a String",
            Self::I32 | Self::F64 => "a Number",
        }
    }
}

/// What a declared host function gives back, and what JavaScript value the
/// call site therefore is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostResult {
    /// No wasm result at all. The call evaluates to `undefined`, which is what
    /// a JavaScript function that returns nothing evaluates to.
    Void,
    /// One raw `i32`, rewrapped as a JavaScript Number.
    I32,
    /// One raw `f64`, rewrapped as a JavaScript Number.
    F64,
    /// A variable-length byte result, fetched in two passes and rewrapped as a
    /// JavaScript String.
    ///
    /// Two imports, not one, because a wasm function cannot return a slice.
    /// `length` names a second field in the same [`HostFn::module`]:
    ///
    /// ```text
    /// <module>.<length>(<declared params...>) -> i32              // bytes available
    /// <module>.<field> (<declared params...>, dst: i32, cap: i32) -> i32  // bytes written
    /// ```
    ///
    /// The compiler calls `length`, bump-allocates a string record of that
    /// size on the guest's own heap, calls `field` to fill it, and **checks**
    /// that the copy wrote exactly what the length promised. A short write, or
    /// the negative answer a "your buffer is too small" contract gives, is a
    /// trap rather than a String with a fabricated tail.
    ///
    /// Both imports receive the declared parameters, so a declaration using
    /// this result shape must be one whose length pass is free of side
    /// effects. In practice such a door takes no arguments at all: the doing
    /// is one declaration and the fetching is another.
    Bytes {
        /// The field name of the length import, in the same module. It must
        /// differ from [`HostFn::field`] -- the two have different signatures,
        /// so they cannot be one import.
        length: String,
    },
}

/// One host function an embedder declares: what a script may call it, which
/// raw wasm import it is, and how the two are bridged.
///
/// ```
/// use tinyvm_qjs::{HostFn, HostParam, HostResult, Names, Options, compile_qjs_m1_with};
///
/// // `sys.log(ptr: i32, len: i32) -> ()`, written `log("hello")` in a script.
/// let log = HostFn {
///     name: "log".to_string(),
///     module: "sys".to_string(),
///     field: "log".to_string(),
///     params: vec![HostParam::StrPtrLen],
///     result: HostResult::Void,
/// };
/// let wasm = compile_qjs_m1_with(
///     "log(\"hello\"); return 0;",
///     Options { names: Names::Declared(vec![log]) },
/// )?;
/// assert!(wasm.starts_with(b"\0asm"));
/// # Ok::<(), tinyvm_qjs::CompileError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFn {
    /// The name a script writes. Usually the same as [`HostFn::field`], and
    /// separate so that an embedder can rename a raw door without renaming
    /// what its scripts call.
    pub name: String,
    /// The wasm import module name, e.g. `"sys"`.
    pub module: String,
    /// The wasm import field name.
    pub field: String,
    /// One entry per JavaScript argument, in order. A [`HostParam`] may occupy
    /// more than one raw parameter -- see [`HostParam::StrPtrLen`].
    pub params: Vec<HostParam>,
    pub result: HostResult,
}

/// What a bare name in the source resolves to.
///
/// The language and its callers genuinely disagree here, and the disagreement
/// is not a matter of strictness. The language has no bindings of its own, so
/// a name resolves to nothing and the honest answer is a capability
/// diagnostic. The [`eval_wasm`](tinyvm::eval_wasm) skin has exactly one
/// binding table -- `eval_wasm`'s `globals` -- so there a name *does* mean
/// something. And an embedder with a real host door has a table of its own,
/// which the door itself must not have to know about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Names {
    /// Rejected: "this engine does not support variable references yet".
    #[default]
    Unbound,
    /// `g` and `g()` both call the import `js.g`. At M0 that import takes no
    /// arguments; at M1 it takes and returns V1 pairs, so it is a door only a
    /// host that speaks this engine's value representation can stand behind.
    HostImport,
    /// A name is one of these declarations, and nothing else is a name.
    ///
    /// The raw mode: each declaration is an ordinary wasm import with an
    /// ordinary wasm signature, and the compiler unwraps JavaScript values
    /// onto it. M1 only -- see [`crate::compile_qjs_m1_with`].
    Declared(Vec<HostFn>),
}

/// How to compile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub names: Names,
}
