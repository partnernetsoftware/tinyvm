//! The [`eval_wasm`](tinyvm::eval_wasm) skin's compile step.
//!
//! [`qjs2wasm`] is [`compile_qjs_with`] under [`Names::HostImport`], with one
//! extra job: narrowing the diagnostic to fit a [`WasmError`].
//!
//! # Why the narrowing exists, and why it lives here
//!
//! [`eval_qjs`](crate::eval_qjs) must return `Result<Vec<Val>, WasmError>`,
//! because the run half is `eval_wasm` and a caller should not have to unify
//! two error types to write one `?`. But `WasmError` carries a `&'static str`
//! and nothing else: the core is `no_std` and fmt-free, which is a product
//! property (a sub-100 KiB static core) and not an oversight.
//!
//! So a rich `String` diagnostic cannot cross this boundary. The tempting
//! alternative — dropping the rich diagnostic and making the compiler speak in
//! `&'static str` — was rejected: the diagnostics *are* the product here. A
//! subset that grows by demand rejects good scripts constantly, and "this
//! engine does not support hexadecimal number literals yet, at byte 4" is the
//! difference between a boundary a reader can see and a boundary they have to
//! guess at.
//!
//! What crosses instead is [`Boundary`](crate::Boundary): the category, chosen
//! at the point the diagnostic is raised, carried as a `&'static str` the core
//! can hold. Callers with a `String` channel should use [`compile_qjs`] and
//! read the sentence.

use tinyvm::WasmError;

use crate::{Names, Options, compile_qjs_with};

/// Pack one expression into a standard `.wasm` guest, for the
/// [`eval_wasm`](tinyvm::eval_wasm) skin.
///
/// Subset: decimal integers; `+` `-` `*` `/` `%`; unary minus; grouping `()`;
/// host names (`g` or `g()` → the zero-argument import `js.g`); `$0`/`$1`/…
/// for this-call locals. Host calls take no arguments — that would be a third
/// world. Anything that needs a JS runtime is rejected.
///
/// The error is the narrowed form; [`compile_qjs`](crate::compile_qjs) returns
/// the full diagnostic, with the offset of the construct it names.
pub fn qjs2wasm(source: &str) -> Result<Vec<u8>, WasmError> {
    compile_qjs_with(
        source,
        Options {
            names: Names::HostImport,
        },
    )
    .map_err(WasmError::from)
}
