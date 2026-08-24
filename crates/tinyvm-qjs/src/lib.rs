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
//! There are two entry points, and for one milestone only.
//!
//! [`compile_qjs`] is M0: decimal integer literals, `+ - * / %`, unary minus,
//! parentheses, and `$N` for the Nth argument of this call. The result is an
//! ordinary wasm module exporting one function named `main`, taking one `i32`
//! parameter per argument the source names and returning `i32`.
//!
//! [`compile_qjs_m1`] is M1: statements, `let`/`const`/`var`, `if`/`while`/
//! `for`, functions with parameters and `return`, strings, and the full
//! operator ladder -- all of it over the V1 value representation, where one
//! JavaScript value is a `(tag: i32, payload: i64)` pair. Its `main` therefore
//! takes *two* wasm parameters per argument and returns two results; [`Value`]
//! is the door.
//!
//! The two exist side by side because M0 has callers that are green on `i32`
//! in and `i32` out, and a representation change is not something to do to a
//! caller silently. When they move, M1 takes the name and this paragraph goes.
//!
//! A bare name is the one thing the two callers disagree about, so it is the
//! one thing [`Options`] chooses — see [`Names`]. Under the default the
//! language has nothing to resolve a name against and says so; under
//! [`Names::HostImport`] a name is a zero-argument `js.<name>` import, which is
//! the world [`eval_qjs`] runs in.
//!
//! Everything else is rejected with a diagnostic that names the engine's
//! boundary rather than blaming the script; see [`CompileError`]. At M0 `/` and
//! `%` diverge from JavaScript on a zero divisor, deliberately and for a
//! documented reason — see the `emit` module. At M1 they do not: there is a
//! real `Infinity` and a real `NaN` for them to be.
//!
//! Full JS is not a converter and is not this crate — yet. The subset grows by
//! real script demand, and every rejection says which construct is ahead of the
//! engine so the boundary is visible instead of guessed at. M1 adds two more
//! stages to the list above, both of them internal: `repr` is the value
//! representation and `runtime` is the guest-side code every compiled module
//! carries, because an operator that dispatches on its operands' types is a
//! call and not an opcode.
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
mod repr;
mod runtime;

pub use diag::{Boundary, CompileError};
pub use qjs2wasm::qjs2wasm;

/// A JavaScript value as a host sees it at the call boundary.
///
/// The M1 entry point ([`compile_qjs_m1`]) compiles to the V1 representation,
/// where one JS value is two wasm values, so a host cannot hand one over as a
/// single [`Val`]. This is the door: [`Value::args`] on the way in,
/// [`Value::returned`] on the way out.
///
/// `String` is a guest pointer into the instance's linear memory, not text.
/// Resolving it needs the instance, which is the caller's to hold.
///
/// A separate type from `repr::HostVal`, which is the same five cases: that
/// one is the compiler's internal vocabulary and this one is public API, and a
/// public re-export would freeze the internal one. The conversion is below and
/// is the only place they meet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    String(i32),
}

impl Value {
    /// Flatten JS values into the wasm arguments a compiled entry point takes.
    pub fn args(values: &[Value]) -> Vec<Val> {
        values
            .iter()
            .flat_map(|v| repr::host_encode(repr::HostVal::from(*v)))
            .collect()
    }

    /// Read back what a compiled entry point returned.
    pub fn returned(vals: &[Val]) -> Result<Value, String> {
        repr::host_decode(vals).map(Value::from)
    }
}

impl From<Value> for repr::HostVal {
    fn from(value: Value) -> Self {
        match value {
            Value::Undefined => repr::HostVal::Undefined,
            Value::Null => repr::HostVal::Null,
            Value::Number(x) => repr::HostVal::Number(x),
            Value::Bool(b) => repr::HostVal::Bool(b),
            Value::String(p) => repr::HostVal::String(p),
        }
    }
}

impl From<repr::HostVal> for Value {
    fn from(value: repr::HostVal) -> Self {
        match value {
            repr::HostVal::Undefined => Value::Undefined,
            repr::HostVal::Null => Value::Null,
            repr::HostVal::Number(x) => Value::Number(x),
            repr::HostVal::Bool(b) => Value::Bool(b),
            repr::HostVal::String(p) => Value::String(p),
        }
    }
}

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

/// Compile `.qjs` source through the M1 front end and the V1 value
/// representation. Compile-only: never executes.
///
/// The milestone the rest of the crate is moving to, and a second entry point
/// only until it is: [`compile_qjs`] is M0 -- one integer expression, `i32` in
/// and `i32` out -- and its callers are green on exactly those terms. This one
/// compiles statements, declarations, control flow, functions and strings, and
/// every value it moves is a V1 pair, so its `main` takes two wasm parameters
/// per JavaScript argument and returns two wasm results. Use [`Value`] to
/// cross that boundary.
///
/// When the M0 path is deleted this becomes `compile_qjs`.
///
/// ```
/// use tinyvm::{Limits, WasmModule};
/// use tinyvm_qjs::{Value, compile_qjs_m1};
///
/// let wasm = compile_qjs_m1("return $0 * 2;").expect("compiles");
/// // `WasmError` has no `Debug` -- the core is fmt-free -- so `ok()` first.
/// let module = WasmModule::from_bytes_with(&wasm, Limits::default())
///     .ok()
///     .expect("clears the load gate");
/// let mut instance = module.instantiate().ok().expect("instantiates");
/// let out = instance
///     .invoke_by_name("main", &Value::args(&[Value::Number(21.0)]))
///     .ok()
///     .expect("runs");
/// assert_eq!(Value::returned(&out), Ok(Value::Number(42.0)));
/// ```
pub fn compile_qjs_m1(source: &str) -> Result<Vec<u8>, CompileError> {
    compile_qjs_m1_with(source, Options::default())
}

/// [`compile_qjs_m1`] with the caller's [`Options`].
///
/// Under [`Names::HostImport`] a free name is an import `js.<name>`, called
/// with the JS values written at the call site and returning one. That is a
/// wider door than M0's zero-argument `i32` one, and it is not the door
/// [`eval_wasm`]'s [`HostGlobal`] fits through -- bind it with
/// `Module::bind_import_typed`.
pub fn compile_qjs_m1_with(source: &str, options: Options) -> Result<Vec<u8>, CompileError> {
    let tokens = lex::tokenize(source)?;
    let program = parse::m1::parse(&tokens, options)?;
    let module = emit::m1::lower(&program)?;
    Ok(ir::m1::assemble(&module))
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
