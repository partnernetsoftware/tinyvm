//! The V1 value representation and its emitted runtime, executed for real.
//!
//! # Why this file compiles the modules itself
//!
//! `src/repr.rs` and `src/runtime.rs` are not yet declared in `src/lib.rs`, and
//! `src/ir.rs` / `src/encode.rs` do not yet carry the `i64`, `f64`, control
//! flow, global and memory that V1 needs -- all three files are outside this
//! lane's domain. So this test includes the two modules by path and prints
//! their IR as WebAssembly text, which the `wat` dev-dependency assembles.
//!
//! That is not a shortcut around the hand-written encoder; it is the same
//! cross-check the acceptance suite already makes, run in the other direction.
//! What matters is that the bytes clear **tinyvm's load gate** and that the
//! functions **return the right answers when run**, and both are checked here.
//! When `encode.rs` grows the instructions, these same tests re-point at it.
//!
//! # What is deliberately different from the experiment
//!
//! - The `Repr` trait is gone. It existed to hold two variants for a
//!   comparison; V1 won, and a one-implementation trait is indirection with no
//!   reader.
//! - `Origin` provenance tagging is gone. It existed to measure criterion 6.
//!   The measurement is done and its numbers are in `RESULTS.md`.
//! - `TAG_NULL = 4` is added. ECMA-262 6.1.2 makes Null its own language type,
//!   and folding it onto Undefined would make `null === undefined` true.
//! - Semantics moved from research-grade to ECMA-262 where no new machinery was
//!   needed: `ToNumber` is a real function, so `1 - true` is `0` and
//!   `undefined + 1` is `NaN`; `===` is complete; `==` bridges `null` and
//!   `undefined`. The three conversions that *do* need new machinery are
//!   `unreachable` arms, listed in `runtime.rs`'s header.
//! - `Ctx::func_base` is new: the experiment's modules had no imports, this
//!   compiler's have `js.<name>`.

#[path = "../src/repr.rs"]
mod repr;
#[path = "../src/runtime.rs"]
mod runtime;

use repr::{BlockType, HostVal, Ins, ValType};
use runtime::{Ctx, FnBuild, Rt, StringPool, TypeNames};
use tinyvm::{Limits, Val, WasmModule};

// =========================================================================
// Harness
// =========================================================================

/// A module with the whole runtime in it and one `main` the test writes.
struct Prog {
    pool: StringPool,
    ctx: Ctx,
    main: FnBuild,
    /// `main`'s results. One JS value unless the test says otherwise.
    results: Vec<ValType>,
}

/// What `main` returned.
#[derive(Debug)]
struct Out {
    value: HostVal,
    /// The resolved text, when the value was a String.
    text: Option<String>,
}

impl Prog {
    fn new() -> Self {
        // `__typeof` answers with pool records, so the pool has to exist
        // before the runtime is built and the five names have to be in it.
        // Every program here carries them, which is the compiler's `typeof`
        // case; the compiler's other case -- `type_names: None`, no `typeof`
        // in the source, no five literals in the data segment -- is what the
        // dispatch-order tests below build directly.
        let mut pool = StringPool::default();
        let type_names = Some(TypeNames::intern(&mut pool));
        Prog {
            pool,
            // No imports in these modules, so the runtime starts at 0, and one
            // global, so the bump pointer is global 0.
            ctx: Ctx {
                func_base: 0,
                heap_global: 0,
                type_names,
            },
            main: FnBuild::new(0),
            results: vec![ValType::I32, ValType::I64],
        }
    }

    fn text(&mut self, s: &str) -> i32 {
        self.pool.intern(s)
    }

    fn call(&mut self, rt: Rt) {
        let ins = self.ctx.call(rt);
        self.main.body.push(ins);
    }

    fn emit(&mut self, f: impl FnOnce(&mut Vec<Ins>)) {
        f(&mut self.main.body);
    }

    fn wat(&self) -> String {
        let funcs = runtime::build(&self.ctx);
        let mut out = String::from("(module\n  (memory 1 16)\n");
        out.push_str(&format!(
            "  (global (mut i32) (i32.const {}))\n",
            self.pool.heap_start()
        ));
        if !self.pool.is_empty() {
            let (offset, bytes) = self.pool.segment();
            out.push_str(&format!("  (data (i32.const {offset}) \""));
            for b in bytes {
                out.push_str(&format!("\\{b:02x}"));
            }
            out.push_str("\")\n");
        }
        for f in &funcs {
            out.push_str(&func_wat(&f.params, &f.results, &f.locals, &f.body));
        }
        out.push_str(&func_wat(
            &[],
            &self.results,
            &self.main.local_groups(),
            &self.main.body,
        ));
        out.push_str(&format!("  (export \"main\" (func {}))\n)\n", funcs.len()));
        out
    }

    fn bytes(&self) -> Vec<u8> {
        wat::parse_str(self.wat()).expect("the printed text is valid wasm text")
    }

    /// Load through tinyvm's gate, instantiate, run.
    fn run(&self) -> Result<Out, String> {
        let bytes = self.bytes();
        let module = WasmModule::from_bytes_with(&bytes, Limits::default())
            .map_err(|e| format!("load gate rejected the module: {}", e.message()))?;
        let mut instance = module
            .instantiate()
            .map_err(|e| format!("instantiate failed: {}", e.message()))?;
        let vals = instance
            .invoke_by_name("main", &[])
            .map_err(|e| format!("trap in main: {}", e.message()))?;
        let value = repr::host_decode(&vals)?;
        let text = match value {
            HostVal::String(ptr) => Some(read_string(&instance, ptr)?),
            _ => None,
        };
        Ok(Out { value, text })
    }

    fn number(&self) -> f64 {
        match self.run().expect("main returns").value {
            HostVal::Number(x) => x,
            other => panic!("expected a Number, got {other:?}"),
        }
    }

    fn boolean(&self) -> bool {
        match self.run().expect("main returns").value {
            HostVal::Bool(b) => b,
            other => panic!("expected a Boolean, got {other:?}"),
        }
    }

    fn string(&self) -> String {
        let out = self.run().expect("main returns");
        match out.value {
            HostVal::String(_) => out.text.expect("string text resolves"),
            other => panic!("expected a String, got {other:?}"),
        }
    }
}

fn read_string(instance: &tinyvm::WasmInstance, ptr: i32) -> Result<String, String> {
    let view = instance
        .memory()
        .map_err(|e| format!("no guest memory: {}", e.message()))?;
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let header = bytes
        .get(at..at + 4)
        .ok_or_else(|| format!("string header at {ptr} is out of bounds"))?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let body = bytes
        .get(at + 4..at + 4 + len)
        .ok_or_else(|| format!("string body at {ptr} (len {len}) is out of bounds"))?;
    String::from_utf8(body.to_vec()).map_err(|_| "string is not valid UTF-8".to_string())
}

// ---- the IR -> wasm text printer ---------------------------------------

fn func_wat(
    params: &[ValType],
    results: &[ValType],
    locals: &[(u32, ValType)],
    body: &[Ins],
) -> String {
    let mut out = String::from("  (func");
    if !params.is_empty() {
        out.push_str(" (param");
        for t in params {
            out.push(' ');
            out.push_str(ty_wat(*t));
        }
        out.push(')');
    }
    if !results.is_empty() {
        out.push_str(" (result");
        for t in results {
            out.push(' ');
            out.push_str(ty_wat(*t));
        }
        out.push(')');
    }
    for (count, ty) in locals {
        for _ in 0..*count {
            out.push_str(&format!(" (local {})", ty_wat(*ty)));
        }
    }
    out.push('\n');
    for ins in body {
        out.push_str("    ");
        out.push_str(&ins_wat(ins));
        out.push('\n');
    }
    out.push_str("  )\n");
    out
}

fn ty_wat(t: ValType) -> &'static str {
    match t {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F64 => "f64",
    }
}

fn block_wat(b: BlockType) -> &'static str {
    match b {
        BlockType::Empty => "",
    }
}

/// `align` is a byte count in the text format and an exponent in the IR.
fn memarg(align: u32, offset: u32) -> String {
    format!(" offset={offset} align={}", 1u32 << align)
}

fn f64_wat(x: f64) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else if x.is_infinite() {
        if x > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        format!("{x:?}")
    }
}

fn ins_wat(ins: &Ins) -> String {
    match ins {
        Ins::Block(b) => format!("block{}", block_wat(*b)),
        Ins::Loop(b) => format!("loop{}", block_wat(*b)),
        Ins::If(b) => format!("if{}", block_wat(*b)),
        Ins::End => "end".into(),
        Ins::Br(d) => format!("br {d}"),
        Ins::BrIf(d) => format!("br_if {d}"),
        Ins::Return => "return".into(),
        Ins::Call(i) => format!("call {i}"),
        Ins::Unreachable => "unreachable".into(),
        Ins::Drop => "drop".into(),
        Ins::LocalGet(i) => format!("local.get {i}"),
        Ins::LocalSet(i) => format!("local.set {i}"),
        Ins::LocalTee(i) => format!("local.tee {i}"),
        Ins::GlobalGet(i) => format!("global.get {i}"),
        Ins::GlobalSet(i) => format!("global.set {i}"),
        Ins::I32Load(a, o) => format!("i32.load{}", memarg(*a, *o)),
        Ins::I32Load8U(a, o) => format!("i32.load8_u{}", memarg(*a, *o)),
        Ins::I32Store(a, o) => format!("i32.store{}", memarg(*a, *o)),
        Ins::I32Store8(a, o) => format!("i32.store8{}", memarg(*a, *o)),
        Ins::MemorySize => "memory.size".into(),
        Ins::MemoryGrow => "memory.grow".into(),
        Ins::I32Const(v) => format!("i32.const {v}"),
        Ins::I64Const(v) => format!("i64.const {v}"),
        Ins::F64Const(v) => format!("f64.const {}", f64_wat(*v)),
        Ins::I32Eqz => "i32.eqz".into(),
        Ins::I32Eq => "i32.eq".into(),
        Ins::I32Ne => "i32.ne".into(),
        Ins::I32GeU => "i32.ge_u".into(),
        Ins::I32Add => "i32.add".into(),
        Ins::I32Sub => "i32.sub".into(),
        Ins::I32Mul => "i32.mul".into(),
        Ins::I32DivS => "i32.div_s".into(),
        Ins::I32RemS => "i32.rem_s".into(),
        Ins::I32And => "i32.and".into(),
        Ins::I32Or => "i32.or".into(),
        Ins::I32Shl => "i32.shl".into(),
        Ins::I64Eq => "i64.eq".into(),
        Ins::F64Eq => "f64.eq".into(),
        Ins::F64Ne => "f64.ne".into(),
        Ins::F64Lt => "f64.lt".into(),
        Ins::F64Gt => "f64.gt".into(),
        Ins::F64Le => "f64.le".into(),
        Ins::F64Ge => "f64.ge".into(),
        Ins::F64Abs => "f64.abs".into(),
        Ins::F64Neg => "f64.neg".into(),
        Ins::F64Add => "f64.add".into(),
        Ins::F64Sub => "f64.sub".into(),
        Ins::F64Mul => "f64.mul".into(),
        Ins::F64Div => "f64.div".into(),
        Ins::F64Copysign => "f64.copysign".into(),
        Ins::I32WrapI64 => "i32.wrap_i64".into(),
        Ins::I64ExtendI32U => "i64.extend_i32_u".into(),
        Ins::F64ConvertI32S => "f64.convert_i32_s".into(),
        Ins::F64ReinterpretI64 => "f64.reinterpret_i64".into(),
        Ins::I64ReinterpretF64 => "i64.reinterpret_f64".into(),
    }
}

// ---- little builders the tests share ------------------------------------

enum V {
    Num(f64),
    Bool(bool),
    Str(&'static str),
    Undefined,
    Null,
}

impl Prog {
    fn value(&mut self, v: &V) {
        match v {
            V::Num(x) => self.emit(|b| repr::const_number(*x, b)),
            V::Bool(t) => self.emit(|b| repr::const_bool(*t, b)),
            V::Str(s) => {
                let ptr = self.text(s);
                self.emit(|b| repr::const_string(ptr, b));
            }
            V::Undefined => self.emit(repr::const_undefined),
            V::Null => self.emit(repr::const_null),
        }
    }

    /// `main` = `op(a, b)`.
    fn binary(op: Rt, a: V, b: V) -> Prog {
        let mut p = Prog::new();
        p.value(&a);
        p.value(&b);
        p.call(op);
        p
    }

    /// `main` = `op(a)`.
    fn unary(op: Rt, a: V) -> Prog {
        let mut p = Prog::new();
        p.value(&a);
        p.call(op);
        p
    }

    /// `main` = `Boolean(a)`. `__truthy` hands back a raw `i32`, so the whole
    /// call becomes the payload of a boxed Boolean and the harness keeps one
    /// shape.
    fn to_boolean(a: V) -> Prog {
        let mut p = Prog::new();
        p.value(&a);
        p.call(Rt::Truthy);
        let inner = std::mem::take(&mut p.main.body);
        repr::box_bool(&inner, &mut p.main.body);
        p
    }
}

// =========================================================================
// The layout and the tag domain
// =========================================================================

#[test]
fn a_value_is_two_words_and_the_tag_domain_is_the_five_language_types() {
    assert_eq!(repr::WIDTH, 2);
    assert_eq!(repr::SLOTS, [ValType::I32, ValType::I64]);
    // The numbering is contract, not detail: 0 so that zeroed storage reads as
    // `undefined`, and 1..3 preserved from the measured experiment.
    assert_eq!(repr::TAG_UNDEFINED, 0);
    assert_eq!(repr::TAG_NUMBER, 1);
    assert_eq!(repr::TAG_BOOL, 2);
    assert_eq!(repr::TAG_STRING, 3);
    assert_eq!(repr::TAG_NULL, 4);
}

#[test]
fn the_host_door_is_bit_exact_in_both_directions() {
    for value in [
        HostVal::Undefined,
        HostVal::Null,
        HostVal::Bool(true),
        HostVal::Bool(false),
        HostVal::Number(0.0),
        HostVal::Number(-0.0),
        HostVal::Number(f64::NAN),
        HostVal::Number(f64::INFINITY),
        HostVal::Number(1.5),
        HostVal::String(64),
    ] {
        let encoded = repr::host_encode(value);
        let back = repr::host_decode(&encoded).expect("decodes");
        match (value, back) {
            // `-0` must not satisfy `+0` and a NaN must keep its payload: the
            // sign of a zero is observable in JavaScript, so a representation
            // that loses it has lost information rather than rounded it.
            (HostVal::Number(a), HostVal::Number(b)) => assert_eq!(a.to_bits(), b.to_bits()),
            (a, b) => assert_eq!(a, b),
        }
    }
}

#[test]
fn undefined_and_null_both_carry_payload_zero() {
    // `__strict_eq` collapses Boolean, Undefined and Null into one `i64.eq` on
    // the payload, which is only sound while this holds.
    for v in [HostVal::Undefined, HostVal::Null] {
        assert!(matches!(repr::host_encode(v)[1], Val::I64(0)));
    }
    let mut body = Vec::new();
    repr::const_undefined(&mut body);
    repr::const_null(&mut body);
    assert_eq!(
        body,
        vec![
            Ins::I32Const(repr::TAG_UNDEFINED),
            Ins::I64Const(0),
            Ins::I32Const(repr::TAG_NULL),
            Ins::I64Const(0),
        ]
    );
}

#[test]
fn host_decode_rejects_a_pair_it_did_not_build() {
    assert!(repr::host_decode(&[Val::I32(99), Val::I64(0)]).is_err());
    assert!(repr::host_decode(&[Val::I32(1)]).is_err());
}

// =========================================================================
// Dispatch order -- a decision, so it gets a lock
// =========================================================================

#[test]
fn add_tests_number_before_string() {
    let ctx = Ctx {
        func_base: 0,
        heap_global: 0,
        type_names: None,
    };
    let funcs = runtime::build(&ctx);
    let add = funcs
        .iter()
        .find(|f| f.name == "__add")
        .expect("__add is in the set");
    // The first thing `__add` does is ask whether both operands are Numbers.
    // The experiment measured the other order at 2 619 extra steps on a corpus
    // with no strings in it (RESULTS.md, sensitivity S-ADD).
    assert_eq!(
        &add.body[..7],
        &[
            Ins::LocalGet(0),
            Ins::I32Const(repr::TAG_NUMBER),
            Ins::I32Eq,
            Ins::LocalGet(2),
            Ins::I32Const(repr::TAG_NUMBER),
            Ins::I32Eq,
            Ins::I32And,
        ]
    );
    let first_string_test = add
        .body
        .iter()
        .position(|i| *i == Ins::I32Const(repr::TAG_STRING))
        .expect("__add has a String arm");
    assert!(first_string_test > 7, "String is tested after Number");
}

#[test]
fn a_new_type_costs_nothing_at_a_site_that_never_sees_it() {
    // Null is the type this milestone added. Nothing that dispatches on Number
    // or String gained an arm for it: the arms live in `__to_number`,
    // `__truthy` and the two equalities, which is where the cost is paid once.
    let ctx = Ctx {
        func_base: 0,
        heap_global: 0,
        type_names: None,
    };
    let arms: Vec<&'static str> = runtime::build(&ctx)
        .iter()
        .filter(|f| {
            // A Null arm is the three-instruction tag test, not the bare
            // constant 4 -- `__str_concat` adds 4 for the length header.
            f.body.windows(3).any(|w| {
                matches!(w[0], Ins::LocalGet(_))
                    && w[1] == Ins::I32Const(repr::TAG_NULL)
                    && w[2] == Ins::I32Eq
            })
        })
        .map(|f| f.name)
        .collect();
    assert_eq!(arms, vec!["__eq", "__to_number", "__truthy"]);
}

// =========================================================================
// The module is a real module
// =========================================================================

#[test]
fn the_emitted_runtime_clears_tinyvms_load_gate() {
    let mut p = Prog::new();
    let _ = p.text("seed");
    p.value(&V::Num(1.0));
    let bytes = p.bytes();
    assert!(bytes.starts_with(b"\0asm"));
    if let Err(e) = WasmModule::from_bytes_with(&bytes, Limits::default()) {
        panic!("the load gate rejected the module: {}", e.message());
    }
}

// =========================================================================
// Boxing
// =========================================================================

#[test]
fn a_number_survives_boxing_bit_exactly() {
    for x in [0.0f64, -0.0, 1.5, -7.0, f64::INFINITY, f64::MIN_POSITIVE] {
        let mut p = Prog::new();
        p.value(&V::Num(x));
        assert_eq!(p.number().to_bits(), x.to_bits(), "for {x}");
    }
}

#[test]
fn unboxing_the_wrong_type_traps_rather_than_fabricating_a_value() {
    // `__len` unboxes a String. Handing it a Number must not read the tag as a
    // pointer: a fabricated answer is indistinguishable from a real one.
    let p = Prog::unary(Rt::Len, V::Num(3.0));
    let err = p.run().expect_err("traps");
    assert!(err.contains("trap in main"), "got {err}");
}

// =========================================================================
// Arithmetic
// =========================================================================

#[test]
fn addition_over_every_pair_the_engine_can_evaluate() {
    let cases: &[(V, V, f64)] = &[
        (V::Num(1.0), V::Num(2.0), 3.0),
        (V::Num(1.0), V::Bool(true), 2.0),
        (V::Bool(true), V::Bool(true), 2.0),
        (V::Num(1.0), V::Null, 1.0),
        (V::Null, V::Null, 0.0),
    ];
    for (a, b, want) in cases {
        let p = Prog::binary(Rt::Add, clone(a), clone(b));
        assert_eq!(p.number(), *want);
    }
    // `undefined` is ToNumber NaN, so it poisons the sum -- per ECMA-262
    // 7.1.4, not by accident.
    let p = Prog::binary(Rt::Add, V::Num(1.0), V::Undefined);
    assert!(p.number().is_nan());
}

#[test]
fn subtraction_multiplication_and_division_go_through_to_number() {
    assert_eq!(
        Prog::binary(Rt::Sub, V::Num(1.0), V::Bool(true)).number(),
        0.0
    );
    assert_eq!(
        Prog::binary(Rt::Mul, V::Num(3.0), V::Num(4.0)).number(),
        12.0
    );
    assert_eq!(
        Prog::binary(Rt::Div, V::Num(3.0), V::Num(2.0)).number(),
        1.5
    );
    // Number is f64, so this is the JavaScript answer, not a trap. That is the
    // whole reason the payload is 64 bits wide.
    assert_eq!(
        Prog::binary(Rt::Div, V::Num(1.0), V::Num(0.0)).number(),
        f64::INFINITY
    );
    assert!(
        Prog::binary(Rt::Div, V::Num(0.0), V::Num(0.0))
            .number()
            .is_nan()
    );
}

/// `__rem` on its own, over the whole binary64 domain rather than the
/// literals the front end can spell. Rust's `%` on `f64` is C's `fmod`, which
/// is the operation ECMA-262 6.1.6.1.6 describes, so it is the oracle here.
#[test]
fn the_remainder_matches_fmod_over_the_whole_domain() {
    let inf = f64::INFINITY;
    let interesting: &[f64] = &[
        0.0,
        -0.0,
        1.0,
        -1.0,
        2.0,
        -2.0,
        3.0,
        -3.0,
        5.5,
        -5.5,
        0.125,
        1e-300,
        1e300,
        // The value a rounded quotient gets wrong: `x % 1000` is 608, and
        // `x - trunc(x / 1000) * 1000` is -512.
        2147483647.0 * 2147483647.0,
        9007199254740993.0,
        f64::MAX,
        f64::MIN_POSITIVE,
        inf,
        -inf,
        f64::NAN,
    ];
    for n in interesting {
        for d in interesting {
            let got = Prog::binary(Rt::Rem, V::Num(*n), V::Num(*d)).number();
            let want = *n % *d;
            assert!(
                (got.is_nan() && want.is_nan()) || got.to_bits() == want.to_bits(),
                "{n} % {d}: want {want}, got {got}"
            );
        }
    }
}

/// `__rem` reaches `__to_number` for a non-Number operand, like every other
/// arithmetic operator, rather than carrying its own coercion arms.
#[test]
fn the_remainder_coerces_through_to_number() {
    assert_eq!(
        Prog::binary(Rt::Rem, V::Num(5.0), V::Bool(true)).number(),
        0.0
    );
    assert_eq!(
        Prog::binary(Rt::Rem, V::Bool(true), V::Num(2.0)).number(),
        1.0
    );
    assert!(
        Prog::binary(Rt::Rem, V::Num(5.0), V::Undefined)
            .number()
            .is_nan()
    );
    // `null` is ToNumber 0, and a zero divisor is NaN.
    assert!(
        Prog::binary(Rt::Rem, V::Num(5.0), V::Null)
            .number()
            .is_nan()
    );
}

/// `__typeof` over every tag, and the one arm nobody guesses: 13.5.3 step 3
/// gives the Null type the name `"object"`.
#[test]
fn typeof_answers_with_the_language_type_name() {
    for (value, want) in [
        (V::Num(1.0), "number"),
        (V::Num(f64::NAN), "number"),
        (V::Str("a"), "string"),
        (V::Str(""), "string"),
        (V::Bool(false), "boolean"),
        (V::Undefined, "undefined"),
        (V::Null, "object"),
    ] {
        assert_eq!(Prog::unary(Rt::TypeOf, value).string(), want);
    }
}

/// A program with no `typeof` in it carries none of the five names: the pool
/// is the data segment, and 64 bytes of guest memory per module is not a cost
/// a script that never asks should pay.
#[test]
fn typeof_costs_nothing_in_a_program_that_never_asks() {
    let mut asks = StringPool::default();
    TypeNames::intern(&mut asks);
    assert!(!asks.is_empty());
    let quiet = StringPool::default();
    assert!(quiet.is_empty());
    assert!(quiet.heap_start() < asks.heap_start());

    // And the emitted function is the trap that says nothing may call it.
    let ctx = Ctx {
        func_base: 0,
        heap_global: 0,
        type_names: None,
    };
    let built = runtime::build(&ctx);
    let quiet_typeof = built
        .iter()
        .find(|f| f.name == "__typeof")
        .expect("__typeof is in the set");
    assert_eq!(quiet_typeof.body, vec![Ins::Unreachable]);
}

#[test]
fn unary_minus_keeps_the_sign_of_zero() {
    assert_eq!(
        Prog::unary(Rt::Neg, V::Num(0.0)).number().to_bits(),
        (-0.0f64).to_bits()
    );
    assert_eq!(
        Prog::unary(Rt::Neg, V::Num(-0.0)).number().to_bits(),
        0.0f64.to_bits()
    );
    assert_eq!(Prog::unary(Rt::Neg, V::Bool(true)).number(), -1.0);
}

#[test]
fn relational_operators_follow_is_less_than() {
    assert!(Prog::binary(Rt::Lt, V::Num(1.0), V::Num(2.0)).boolean());
    assert!(!Prog::binary(Rt::Lt, V::Num(2.0), V::Num(1.0)).boolean());
    assert!(Prog::binary(Rt::Le, V::Num(2.0), V::Num(2.0)).boolean());
    assert!(Prog::binary(Rt::Gt, V::Num(2.0), V::Num(1.0)).boolean());
    assert!(Prog::binary(Rt::Ge, V::Bool(true), V::Num(1.0)).boolean());
    // Every relational comparison with NaN is false, including `>=`.
    for op in [Rt::Lt, Rt::Le, Rt::Gt, Rt::Ge] {
        assert!(!Prog::binary(op, V::Undefined, V::Num(1.0)).boolean());
    }
}

// =========================================================================
// Strings
// =========================================================================

#[test]
fn a_string_literal_comes_back_as_its_text() {
    let mut p = Prog::new();
    p.value(&V::Str("hello"));
    assert_eq!(p.string(), "hello");
}

#[test]
fn concatenation_allocates_a_new_string_on_the_bump_heap() {
    let p = Prog::binary(Rt::Add, V::Str("foo"), V::Str("bar"));
    let out = p.run().expect("runs");
    assert_eq!(out.text.as_deref(), Some("foobar"));
    match out.value {
        // Above the literal pool: this record was allocated, not interned.
        HostVal::String(ptr) => assert!(ptr >= 8, "pointer {ptr} is in the heap"),
        other => panic!("expected a String, got {other:?}"),
    }
}

#[test]
fn concatenation_of_empty_strings_is_still_a_string() {
    assert_eq!(Prog::binary(Rt::Add, V::Str(""), V::Str("")).string(), "");
    assert_eq!(Prog::binary(Rt::Add, V::Str(""), V::Str("x")).string(), "x");
}

#[test]
fn length_is_a_number() {
    assert_eq!(Prog::unary(Rt::Len, V::Str("hello")).number(), 5.0);
    assert_eq!(Prog::unary(Rt::Len, V::Str("")).number(), 0.0);
}

#[test]
fn the_allocator_grows_memory_rather_than_trapping_at_the_page_boundary() {
    // One page is 65 536 bytes. Concatenate a 4 KiB string with itself until
    // well past that, so `memory.grow` has to fire.
    let long = "x".repeat(4096);
    let mut p = Prog::new();
    let ptr = p.pool.intern(&long);
    p.emit(|b| repr::const_string(ptr, b));
    for _ in 0..5 {
        let again = p.pool.intern(&long);
        p.emit(|b| repr::const_string(again, b));
        p.call(Rt::Add);
    }
    p.call(Rt::Len);
    assert_eq!(p.number(), 4096.0 * 6.0);
}

// =========================================================================
// Equality
// =========================================================================

#[test]
fn strict_equality_is_complete_over_the_five_types() {
    let cases: &[(V, V, bool)] = &[
        (V::Num(1.0), V::Num(1.0), true),
        (V::Num(1.0), V::Num(2.0), false),
        // `+0 === -0` is true and `NaN === NaN` is false: this is exactly why
        // the Number arm is `f64.eq` and not a payload compare.
        (V::Num(0.0), V::Num(-0.0), true),
        (V::Undefined, V::Undefined, true),
        (V::Null, V::Null, true),
        // ...and exactly why Null is its own tag.
        (V::Null, V::Undefined, false),
        (V::Bool(true), V::Bool(true), true),
        (V::Bool(true), V::Bool(false), false),
        (V::Str("ab"), V::Str("ab"), true),
        (V::Str("ab"), V::Str("ac"), false),
        (V::Str("ab"), V::Str("abc"), false),
        (V::Num(1.0), V::Str("1"), false),
        (V::Num(1.0), V::Bool(true), false),
        (V::Num(0.0), V::Null, false),
    ];
    for (a, b, want) in cases {
        let got = Prog::binary(Rt::StrictEq, clone(a), clone(b)).boolean();
        assert_eq!(got, *want, "=== on {a:?} and {b:?}");
        let got = Prog::binary(Rt::StrictNe, clone(a), clone(b)).boolean();
        assert_eq!(got, !*want, "!== on {a:?} and {b:?}");
    }
}

#[test]
fn strict_string_equality_compares_bytes_not_pointers() {
    // Two records at two addresses: one interned literal, one allocated by
    // `__str_concat`.
    let mut p = Prog::new();
    p.value(&V::Str("ab"));
    p.value(&V::Str("a"));
    p.value(&V::Str("b"));
    p.call(Rt::Add);
    p.call(Rt::StrictEq);
    assert!(p.boolean());
}

#[test]
fn loose_equality_bridges_null_and_undefined_and_nothing_else() {
    let cases: &[(V, V, bool)] = &[
        (V::Null, V::Undefined, true),
        (V::Undefined, V::Null, true),
        (V::Null, V::Null, true),
        (V::Null, V::Num(0.0), false),
        (V::Undefined, V::Bool(false), false),
        (V::Num(1.0), V::Bool(true), true),
        (V::Num(0.0), V::Bool(false), true),
        (V::Num(2.0), V::Bool(true), false),
        (V::Str("a"), V::Str("a"), true),
    ];
    for (a, b, want) in cases {
        let got = Prog::binary(Rt::Eq, clone(a), clone(b)).boolean();
        assert_eq!(got, *want, "== on {a:?} and {b:?}");
        let got = Prog::binary(Rt::Ne, clone(a), clone(b)).boolean();
        assert_eq!(got, !*want, "!= on {a:?} and {b:?}");
    }
}

// =========================================================================
// ToBoolean
// =========================================================================

#[test]
fn to_boolean_is_complete_over_the_five_types() {
    let cases: &[(V, bool)] = &[
        (V::Num(1.0), true),
        (V::Num(-1.0), true),
        (V::Num(0.0), false),
        (V::Num(-0.0), false),
        // NaN is the falsy Number that is not a zero, which is why the Number
        // arm needs the value three times.
        (V::Num(f64::NAN), false),
        (V::Num(f64::INFINITY), true),
        (V::Bool(true), true),
        (V::Bool(false), false),
        (V::Str("a"), true),
        (V::Str(""), false),
        (V::Undefined, false),
        (V::Null, false),
    ];
    for (v, want) in cases {
        assert_eq!(
            Prog::to_boolean(clone(v)).boolean(),
            *want,
            "ToBoolean({v:?})"
        );
    }
}

// =========================================================================
// The capability boundary
// =========================================================================

#[test]
fn the_three_unimplemented_conversions_trap_instead_of_guessing() {
    // ToString of a Number: `"a" + 1`.
    assert!(
        Prog::binary(Rt::Add, V::Str("a"), V::Num(1.0))
            .run()
            .is_err()
    );
    assert!(
        Prog::binary(Rt::Add, V::Num(1.0), V::Str("a"))
            .run()
            .is_err()
    );
    // StringToNumber: `"1" - 1`, and `1 == "1"`.
    assert!(
        Prog::binary(Rt::Sub, V::Str("1"), V::Num(1.0))
            .run()
            .is_err()
    );
    assert!(
        Prog::binary(Rt::Eq, V::Num(1.0), V::Str("1"))
            .run()
            .is_err()
    );
    // String relational comparison: `"a" < "b"`.
    assert!(
        Prog::binary(Rt::Lt, V::Str("a"), V::Str("b"))
            .run()
            .is_err()
    );
    // But `===` never coerces, so it answers all of these without trapping.
    assert!(!Prog::binary(Rt::StrictEq, V::Num(1.0), V::Str("1")).boolean());
}

// ---- small helpers ------------------------------------------------------

fn clone(v: &V) -> V {
    match v {
        V::Num(x) => V::Num(*x),
        V::Bool(b) => V::Bool(*b),
        V::Str(s) => V::Str(s),
        V::Undefined => V::Undefined,
        V::Null => V::Null,
    }
}

impl std::fmt::Debug for V {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            V::Num(x) => write!(f, "{x}"),
            V::Bool(b) => write!(f, "{b}"),
            V::Str(s) => write!(f, "{s:?}"),
            V::Undefined => write!(f, "undefined"),
            V::Null => write!(f, "null"),
        }
    }
}

/// Print the whole emitted runtime as wasm text. Not an assertion -- a way to
/// read what the lowering will be calling into:
/// `cargo test -p tinyvm-qjs --test repr_v1 -- --ignored --nocapture dump`.
#[test]
#[ignore = "a dump, not a check"]
fn dump_the_emitted_runtime() {
    let mut p = Prog::new();
    p.value(&V::Str("hi"));
    println!("{}", p.wat());
}
