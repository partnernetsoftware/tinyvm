//! The `.qjs` -> `.wasm` compiler, in pure Rust, plus the [`eval_wasm`] skin
//! that runs what it produces.
//!
//! ```text
//! source  --lex-->  tokens  --parse-->  AST  --emit-->  wasm IR  --encode-->  bytes
//! ```
//!
//! Five stages, five modules, each with one job. That is more structure than
//! today's arithmetic needs and exactly the structure the language needs next:
//! strings, objects and closures each land in one or two of these stages, and
//! none of them wants to be threaded through a single pass that turns
//! characters straight into bytes.
//!
//! # Where this crate sits
//!
//! `tinyvm` is the core: wasm decode, validation, [`Limits`](tinyvm::Limits),
//! the host door. This crate is the *language* layer above it, and it is
//! generic: nothing here knows about any particular embedder's host door or
//! business vocabulary. An embedder that wants its own door builds it on
//! `tinyvm` and calls [`compile_qjs`] for the source half.
//!
//! # What it compiles
//!
//! Decimal integer literals, `+ - * / %`, unary minus, parentheses, and `$N`
//! for the Nth argument of this call. The result is an ordinary wasm module
//! exporting one function named `main`, taking one `i32` parameter per argument
//! the source names and returning `i32`.
//!
//! A bare name is the one thing the two callers disagree about, so it is the
//! one thing [`Options`] chooses — see [`Names`]. Under the default the
//! language has nothing to resolve a name against and says so; under
//! [`Names::HostImport`] a name is a zero-argument `js.<name>` import, which is
//! the world [`eval_qjs`] runs in.
//!
//! Everything else is rejected with a diagnostic that names the engine's
//! boundary rather than blaming the script; see [`CompileError`]. `/` and `%`
//! diverge from JavaScript on a zero divisor, deliberately and for a documented
//! reason — see the `emit` module.
//!
//! Full JS is not a converter and is not this crate — yet. The subset grows by
//! real script demand, and every rejection says which construct is ahead of the
//! engine so the boundary is visible instead of guessed at.
//!
//! Commissar demo: `cargo run -p tinyvm-qjs --example commissar`

use tinyvm::{HostGlobal, Val, WasmError, eval_wasm};

mod ast;
mod diag;
mod emit;
mod encode;
mod ir;
mod lex;
mod parse;
mod qjs2wasm;

pub use diag::{Boundary, CompileError};
pub use qjs2wasm::qjs2wasm;

/// What a bare name in the source resolves to.
///
/// The language and the [`eval_wasm`] skin genuinely disagree here, and the
/// disagreement is not a matter of strictness. The language has no bindings
/// yet, so a name resolves to nothing and the honest answer is a capability
/// diagnostic. The skin has exactly one binding table — `eval_wasm`'s
/// `globals` — so there a name *does* mean something, and pretending otherwise
/// would delete the skin's only vocabulary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Names {
    /// Rejected: "this engine does not support variable references yet".
    #[default]
    Unbound,
    /// `g` and `g()` both call the zero-argument import `js.g`. Host calls take
    /// no arguments — that would need a third world beyond the two bindings.
    HostImport,
}

/// How to compile. One field today; the type exists so the next choice is an
/// added field rather than an added function.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub names: Names,
}

/// Compile `.qjs` source to standard wasm bytes. Compile-only: never executes.
///
/// The bytes are an ordinary module. They go through tinyvm's load gate on the
/// same terms as a hand-written `.wasm` guest, which is the point: there is one
/// engine here, not two pipelines sharing a name.
///
/// ```
/// let wasm = tinyvm_qjs::compile_qjs("$0*2").expect("compiles");
/// assert!(wasm.starts_with(b"\0asm"));
/// ```
pub fn compile_qjs(source: &str) -> Result<Vec<u8>, CompileError> {
    compile_qjs_with(source, Options::default())
}

/// [`compile_qjs`] with the caller's [`Options`].
pub fn compile_qjs_with(source: &str, options: Options) -> Result<Vec<u8>, CompileError> {
    let tokens = lex::tokenize(source)?;
    let program = parse::parse(&tokens, options)?;
    let module = emit::lower(&program);
    Ok(encode::encode(&module))
}

/// [`qjs2wasm`] then [`eval_wasm`]: `eval_wasm(&qjs2wasm(source)?, globals, locals)`.
///
/// The world is only those two bindings: `globals` is the import table a name
/// resolves against, `locals` are this call's `$N`.
///
/// ```
/// use tinyvm::{HostGlobal, Val};
/// use tinyvm_qjs::eval_qjs;
/// let g = [HostGlobal::new("js", "g", Val::I32(40))];
/// let got = eval_qjs("g()+$0", &g, &[Val::I32(2)]);
/// assert!(matches!(got, Ok(vals) if matches!(vals.as_slice(), [Val::I32(42)])));
/// ```
pub fn eval_qjs(
    source: &str,
    globals: &[HostGlobal<'_>],
    locals: &[Val],
) -> Result<Vec<Val>, WasmError> {
    eval_wasm(&qjs2wasm(source)?, globals, locals)
}
