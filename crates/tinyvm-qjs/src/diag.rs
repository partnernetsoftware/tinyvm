//! Compile diagnostics.
//!
//! Every message speaks for the *engine*, never about the author. That is a
//! product requirement, not a style preference: this compiler's subset is
//! deliberately small and grows by real script demand, so the overwhelmingly
//! common rejection is a perfectly good script that is simply ahead of the
//! engine. Telling that author "syntax error" would be a lie.
//!
//! Two constructors, and the distinction between them is the whole point:
//!
//! * [`unsupported`] -- the construct is real JavaScript that this engine does
//!   not lower yet. The wording is fixed ("this engine does not support X
//!   yet") and locked by `tests/qjs_subset.rs`, because that wording is the
//!   thing product documentation is not allowed to drift away from.
//! * [`malformed`] -- the source is structurally incomplete (ends mid
//!   expression, an unclosed group). Free wording, but still narrated from the
//!   engine's side: what it was looking for and did not find.
//!
//! Each also carries a [`Boundary`], which is the machine-readable half of the
//! same fact; see that type for why a `String` alone is not enough.

/// Which kind of boundary a rejection ran into, in a form that survives a
/// caller with no room for a `String`.
///
/// [`tinyvm::WasmError`] holds a `&'static str` and nothing else -- the core is
/// `no_std` and fmt-free by design. So [`crate::qjs2wasm`], which must return
/// that type, cannot carry a [`CompileError`]'s sentence out. Rather than
/// re-deriving the category by matching on the sentence downstream (the exact
/// habit `WasmError::class` exists to kill), every diagnostic declares its
/// category here and hands the fmt-free caller [`Boundary::terse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Real JavaScript whose meaning needs a runtime this compiler does not
    /// emit yet -- a keyword, `eval`, the `undefined` value.
    FullJs,
    /// A construct that would need a third world beyond the two bindings a
    /// [`crate::eval_qjs`] call has: the host import table and this call's
    /// arguments. Property access, arrays, objects, host calls with arguments.
    ThirdBinding,
    /// Inside the expression subset in shape, but not lowered yet -- or source
    /// the engine cannot read to the end.
    Subset,
}

impl Boundary {
    /// The fmt-free summary of this boundary.
    ///
    /// Deliberately not a re-wording of the rich sentence: it is the most a
    /// `&'static str` channel can carry, and callers with a `String` channel
    /// should read [`CompileError::message`] instead.
    pub fn terse(self) -> &'static str {
        match self {
            Self::FullJs => "full JS needs a runtime this engine does not emit yet",
            Self::ThirdBinding => "the world is only the two bindings; this needs a third",
            Self::Subset => "outside the expression subset this engine lowers",
        }
    }
}

/// A compile failure that names the engine's capability boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct CompileError {
    /// The whole sentence, phrased as what the engine cannot do yet.
    pub message: String,
    /// Where the construct starts, in bytes from the start of the source.
    pub offset: usize,
    /// The same fact, coarse enough to cross a fmt-free boundary.
    pub boundary: Boundary,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for CompileError {}

impl From<CompileError> for tinyvm::WasmError {
    /// Narrow a diagnostic to what the fmt-free core can hold. Lossy on
    /// purpose and in one place, so no caller invents its own narrowing.
    fn from(error: CompileError) -> Self {
        tinyvm::WasmError::Decode(error.boundary.terse())
    }
}

/// "this engine does not support {construct} yet".
///
/// `construct` is a noun phrase naming the capability, e.g. `"string
/// literals"`, ``"the `let` keyword"``. It must read naturally in that
/// sentence, and it must be specific enough that a reader can tell which part
/// of their script is ahead of the engine.
pub(crate) fn unsupported(boundary: Boundary, construct: &str, offset: usize) -> CompileError {
    CompileError {
        message: format!("this engine does not support {construct} yet"),
        offset,
        boundary,
    }
}

/// "this engine {what}" -- against the host table an embedder declared.
///
/// A third constructor because these rejections are neither of the other two.
/// [`unsupported`] says a construct is ahead of the engine, and nothing here
/// is: `log(1)` is a call this engine can lower, to a door that does not take
/// a Number. [`malformed`] says the source cannot be read to the end, and it
/// reads fine. What went wrong is that the script asked the embedder's host
/// table for something it does not contain, or asked for it in a shape it
/// cannot take -- or that the table itself cannot be an import table.
///
/// Still the engine's voice, and always [`Boundary::ThirdBinding`], because
/// the host table is exactly the third world that boundary names.
///
/// `what` is a verb phrase completing "this engine {what}", e.g. ``"has no
/// host function named `x`"``.
pub(crate) fn host_table(what: &str, offset: usize) -> CompileError {
    CompileError {
        message: format!("this engine {what}"),
        offset,
        boundary: Boundary::ThirdBinding,
    }
}

/// "this engine {what}" -- for input the engine cannot finish reading.
///
/// `what` is a verb phrase completing that sentence, e.g. `"needs an operand
/// after the operator here; the source ends first"`.
pub(crate) fn malformed(what: &str, offset: usize) -> CompileError {
    CompileError {
        message: format!("this engine {what}"),
        offset,
        boundary: Boundary::Subset,
    }
}
