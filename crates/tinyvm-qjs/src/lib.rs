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
//! A bare name is the one thing this compiler's callers disagree about, so it
//! is the one thing [`Options`] chooses — see [`Names`]. Under the default the
//! language has nothing to resolve a name against and says so; under
//! [`Names::HostImport`] a name is a `js.<name>` import in this engine's own
//! value representation, which is the world [`eval_qjs`] runs in; under
//! [`Names::Declared`] a name is one of the embedder's own host functions,
//! reached through an ordinary wasm import with an ordinary wasm signature.
//!
//! # The host door stays raw
//!
//! [`Names::Declared`] is how a compiled `.qjs` script reaches a host
//! capability *with arguments*, and the shape of it is a decision worth
//! reading before using it. An embedder declares raw wasm functions — module,
//! field, signature — and says how each JavaScript argument maps onto raw
//! parameters ([`HostFn`], [`HostParam`], [`HostResult`]). **The compiler
//! unwraps; the door does not learn about JavaScript values.** A String
//! argument becomes a `(ptr, len)` pair into linear memory; a byte result
//! becomes a String again through a two-pass read onto the guest's own heap.
//!
//! That direction is the point. A door that spoke `(tag: i32, payload: i64)`
//! would break every hand-written `.wasm` guest that already stands behind it,
//! and would leak one language's value representation into a boundary meant to
//! serve any guest. So the language layer's job is the *mechanism*, and the
//! embedder declares what exists: nothing in this crate names anybody's host
//! function, and nothing in it should.
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

// Q1 of the method-binding track: three ways of changing this compiler, so
// they are features rather than three prototypes -- see
// `plan/design-method-binding-experiment.md` §5. Exactly one at a time, or the
// measurement is of a chimera rather than of a variant. All three are deleted
// when Q1 is decided; the winner survives as an ordinary implementation.
#[cfg(any(
    all(feature = "method-this", feature = "method-bound"),
    all(feature = "method-this", feature = "method-callsite"),
    all(feature = "method-bound", feature = "method-callsite"),
))]
compile_error!(
    "the method-binding variants are mutually exclusive: enable exactly one of      `method-this`, `method-bound`, `method-callsite` -- two at once would      measure neither"
);

mod array;
mod ast;
/// Research only -- Q1 variant C. Deleted when the track is decided.
#[cfg(any(
    feature = "method-callsite",
    feature = "method-bound",
    feature = "method-this"
))]
mod method;
mod convert;
mod diag;
mod emit;
mod encode;
mod ir;
mod lex;
mod opts;
mod parse;
mod qjs2wasm;
mod repr;
mod runtime;

pub use diag::{Boundary, CompileError};
pub use opts::{HostFn, HostParam, HostResult, Names, Options};
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

/// What the guest wrote down about its own failure, before it trapped.
///
/// A compiled `.qjs` guest fails through `unreachable`, and every
/// `unreachable` reaches the host as the same [`tinyvm::WasmError`] with the
/// same [`tinyvm::FaultClass::Guest`] class. That is correct as far as the VM
/// is concerned -- the guest executed an `unreachable` -- and useless to a host
/// that has to decide whether to raise a budget or tell an author their script
/// is broken.
///
/// One of those failures is not the script's fault. A refused `memory.grow`
/// returns `-1` rather than trapping (standard wasm; see
/// `crates/tinyvm/src/wasm.rs`, `Op::MemoryGrow`), so the refusal carries no
/// reason and the allocator has nowhere to put one. It therefore writes the
/// reason into its own linear memory before failing, and this is the name of
/// what it wrote.
///
/// Not exhaustive: a later milestone may record more reasons at the same word,
/// and a host that matches on this must keep a fallback arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuestFault {
    /// The guest's bump heap could not grow. `memory.grow` was refused --
    /// either by [`tinyvm::Limits::max_memory_pages`] or by the module's own
    /// declared maximum or the allocator -- so the script asked for more
    /// memory than this embedding allows, which is a budget fact and not a
    /// defect. Raising the ceiling may let the same script through.
    HeapExhausted,
    /// The script threw a value and nothing caught it. ECMA-262 says the
    /// program terminates with that exception, so the script ran exactly as
    /// written: this is neither a budget to raise nor a defect to report to
    /// the author. It is the third thing the fault word exists to keep apart
    /// from the other two.
    ///
    /// The thrown *value* does not come with it. A compiled module exports no
    /// global, so the pair holding it is not readable from outside; handing it
    /// out would mean exporting an engine-internal value or widening the entry
    /// point's results, and both are decisions about the host boundary rather
    /// than about throwing.
    UncaughtThrow,
}

/// Read the guest's own account of why it trapped, out of its linear memory.
///
/// Call it after an invocation returned `Err`, with the instance's memory
/// zero:
///
/// ```
/// # use tinyvm::{Limits, WasmModule};
/// # use tinyvm_qjs::{GuestFault, Value, compile_qjs_m1};
/// # let wasm = compile_qjs_m1("return 1;").expect("compiles");
/// # let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
/// # let mut instance = module.instantiate().expect("instantiates");
/// if let Err(fault) = instance.invoke_by_name("main", &Value::args(&[])) {
///     let memory = instance.memory().expect("memory zero");
///     match tinyvm_qjs::guest_fault(&memory) {
///         Some(GuestFault::HeapExhausted) => { /* raise the budget */ }
///         Some(GuestFault::UncaughtThrow) => { /* the script threw; report it */ }
///         _ => { /* the script itself is at fault: report `fault` */ }
///     }
/// }
/// ```
///
/// `None` means the guest recorded nothing, which is the honest answer for
/// three different situations and the host should treat them alike: the trap
/// was an ordinary guest fault, or the module is too small to have a fault word
/// (an M0 module from [`compile_qjs`] has no linear memory at all), or the call
/// never started. In none of them did the heap run out and in none of them did
/// the script throw.
///
/// The entry point clears the word on the way in, so the answer is about the
/// most recent call and not an older one.
pub fn guest_fault(memory: &[u8]) -> Option<GuestFault> {
    let at = runtime::FAULT_WORD as usize;
    let word = memory.get(at..at + 4)?;
    let code = i32::from_le_bytes([word[0], word[1], word[2], word[3]]);
    match code {
        runtime::FAULT_HEAP_EXHAUSTED => Some(GuestFault::HeapExhausted),
        runtime::FAULT_UNCAUGHT_THROW => Some(GuestFault::UncaughtThrow),
        _ => None,
    }
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
/// let module =
///     WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the load gate");
/// let mut instance = module.instantiate().expect("instantiates");
/// let out = instance
///     .invoke_by_name("main", &Value::args(&[Value::Number(21.0)]))
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
///
/// Under [`Names::Declared`] a free name is one of the embedder's [`HostFn`]
/// declarations, and the import is the raw wasm function that declaration
/// names -- no V1 pairs at the boundary at all. Only the declarations a script
/// mentions become imports, and they appear in declaration order, so an
/// embedder can predict its own import table without reading the script.
///
/// ```
/// use tinyvm::{Limits, WasmModule};
/// use tinyvm_qjs::{HostFn, HostParam, HostResult, Names, Options, compile_qjs_m1_with};
///
/// let table = vec![
///     // `sys.print(ptr: i32, len: i32) -> ()`
///     HostFn {
///         name: "print".to_string(),
///         module: "sys".to_string(),
///         field: "print".to_string(),
///         params: vec![HostParam::StrPtrLen],
///         result: HostResult::Void,
///     },
///     // `sys.read_len() -> i32` and `sys.read(dst: i32, cap: i32) -> i32`,
///     // written `read()` in a script and answering with a String.
///     HostFn {
///         name: "read".to_string(),
///         module: "sys".to_string(),
///         field: "read".to_string(),
///         params: Vec::new(),
///         result: HostResult::Bytes { length: "read_len".to_string() },
///     },
/// ];
/// let wasm = compile_qjs_m1_with(
///     "print(\"ready\"); return read();",
///     Options { names: Names::Declared(table) },
/// )?;
/// let module = WasmModule::from_bytes_with(&wasm, Limits::default()).unwrap();
/// let imports: Vec<String> = module
///     .imports()
///     .iter()
///     .map(|i| format!("{}.{}", i.module, i.field))
///     .collect();
/// assert_eq!(imports, ["sys.print", "sys.read_len", "sys.read"]);
/// # Ok::<(), tinyvm_qjs::CompileError>(())
/// ```
pub fn compile_qjs_m1_with(source: &str, options: Options) -> Result<Vec<u8>, CompileError> {
    let tokens = lex::tokenize(source)?;
    let program = parse::m1::parse(&tokens, options.clone())?;
    let module = emit::m1::lower(&program, &options)?;
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
