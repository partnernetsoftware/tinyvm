//! The three ECMA-262 conversions in `src/convert.rs`, executed for real.
//!
//! # Why this file compiles the modules itself
//!
//! `src/convert.rs` is not yet declared in `src/lib.rs` and `src/emit.rs` does
//! not yet call it -- both are outside this lane's file domain, so the wiring
//! is a hook the integrator makes and this test stands in for it. It includes
//! `repr`, `runtime` and `convert` by path, prints their IR as WebAssembly
//! text and lets the `wat` dev-dependency assemble it, exactly as
//! `tests/repr_v1.rs` does. What is checked is what matters either way: the
//! bytes clear **tinyvm's load gate**, and the functions **return the right
//! answers when run**.
//!
//! # The oracle, and where the oracle is wrong
//!
//! Rust's `f64` `Display`/`LowerExp` is also a shortest-round-trip formatter,
//! so it is a legitimate oracle for *k* (how many digits) and *n* (where the
//! point goes). It is **not** ECMA-262:
//!
//! - It never uses exponential form, so `1e21` prints as 22 digits and `1e-7`
//!   as `0.0000001`. 6.1.6.1.20 steps 6 to 9 switch at exactly `n > 21` and
//!   `n <= -6`. So the layout is re-derived here from the spec text rather
//!   than compared against Rust's.
//! - At an **exact tie** it picks the odd `s`; the spec's step 5 ends "choose
//!   the one that is even". `785068460487425.25` is such a value: Rust prints
//!   `785068460487425.3`, V8 prints `785068460487425.2`, and the spec sentence
//!   says the latter. Ties are common -- 266 of them in a 520 000-value sweep
//!   of the reference model -- so this is checked rather than waved at:
//!   [`verify_step5`] proves the tie exactly, from the *exact* decimal
//!   expansion of the double, before accepting a digit Rust disagrees with.

// Three source modules included by path, each of which this file uses only
// part of -- `repr`'s host door and `runtime`'s operators have their own test
// file. Dead-code is therefore about the *reader* here and not about the
// module, so it is allowed on the `mod` and nowhere wider.
#[allow(dead_code)]
#[path = "../src/array.rs"]
mod array;
#[path = "../src/convert.rs"]
mod convert;
#[allow(dead_code)]
#[path = "../src/repr.rs"]
mod repr;
#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use convert::Cv;
use repr::{BlockType, Ins, ValType};
use runtime::{Conversions, Ctx as RtCtx, FnBuild, PrimNames, RtFunc, StringPool, TypeNames};
use tinyvm::{Limits, Val, WasmModule};

// =========================================================================
// Harness
// =========================================================================

/// A module carrying the runtime and the conversions, plus one `main` the
/// test writes.
struct Prog {
    pool: StringPool,
    rt: RtCtx,
    cv: convert::Ctx,
    main: FnBuild,
    params: Vec<ValType>,
    results: Vec<ValType>,
}

impl Prog {
    /// The conversions are placed *after* the runtime, so `Rt`'s indices are
    /// untouched and `__alloc` is reached at its existing offset.
    fn new(params: Vec<ValType>, results: Vec<ValType>) -> Self {
        let mut pool = StringPool::default();
        let type_names = Some(TypeNames::intern(&mut pool));
        let prim_names = PrimNames::intern(&mut pool);
        let names = convert::Names::intern(&mut pool);
        let base = runtime::SET.len() as u32;
        let rt = RtCtx {
            object_names: None,
            refusal_names: None,
            call_check: None,
            func_base: 0,
            heap_global: 0,
            type_names,
            prim_names,
            conversions: Conversions {
                num_to_string: base + Cv::NumToString.offset(),
                str_to_num: base + Cv::StrToNum.offset(),
                str_cmp: base + Cv::StrCmp.offset(),
            },
            // Built directly rather than through the lowering, so this picks
            // the gate a program with no array would have picked.
            arrays: false,
            // These build the runtime directly; no program, so nothing captures.
            captures: false,
            string_length: None,
            string_member: false,
            unwind: None,
            type_error: None,
        };
        let cv = convert::Ctx {
            func_base: base,
            runtime_base: 0,
            names,
        };
        Prog {
            pool,
            rt,
            cv,
            main: FnBuild::new(params.len() as u32),
            params,
            results,
        }
    }

    fn call(&mut self, cv: Cv) {
        let ins = self.cv.call(cv);
        self.main.body.push(ins);
    }

    fn emit(&mut self, run: &[Ins]) {
        self.main.body.extend_from_slice(run);
    }

    fn funcs(&self) -> Vec<RtFunc> {
        let mut all = runtime::build(&self.rt);
        all.extend(convert::build(&self.cv));
        all
    }

    fn wat(&self) -> String {
        let funcs = self.funcs();
        let mut out = String::from("(module\n  (memory 1 200)\n");
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
            &self.params,
            &self.results,
            &self.main.local_groups(),
            &self.main.body,
        ));
        // A second export that puts the bump pointer back where it started.
        // The heap never frees, so a test that converts thousands of values
        // needs either a fresh instance per value or this; the guest allocates
        // a few kilobytes per call, so resetting is sound as long as nothing
        // the host wrote lives below the mark.
        out.push_str(&format!(
            "  (func\n    i32.const {}\n    global.set 0\n  )\n",
            self.pool.heap_start()
        ));
        out.push_str(&format!("  (export \"main\" (func {}))\n", funcs.len()));
        out.push_str(&format!(
            "  (export \"reset\" (func {}))\n)\n",
            funcs.len() + 1
        ));
        out
    }

    fn bytes(&self) -> Vec<u8> {
        wat::parse_str(self.wat()).expect("the printed text is valid wasm text")
    }

    /// The same module with one conversion's body replaced by a single
    /// `unreachable`, so the difference is that body's encoded size.
    fn bytes_without(&self, drop: Cv) -> Vec<u8> {
        let mut funcs = self.funcs();
        let at = runtime::SET.len() + drop.offset() as usize;
        funcs[at].body = vec![Ins::Unreachable];
        funcs[at].locals = Vec::new();
        let mut out = String::from("(module\n  (memory 1 200)\n");
        out.push_str(&format!(
            "  (global (mut i32) (i32.const {}))\n",
            self.pool.heap_start()
        ));
        let (offset, bytes) = self.pool.segment();
        out.push_str(&format!("  (data (i32.const {offset}) \""));
        for b in bytes {
            out.push_str(&format!("\\{b:02x}"));
        }
        out.push_str("\")\n");
        for f in &funcs {
            out.push_str(&func_wat(&f.params, &f.results, &f.locals, &f.body));
        }
        out.push_str(&func_wat(
            &self.params,
            &self.results,
            &self.main.local_groups(),
            &self.main.body,
        ));
        out.push_str(")\n");
        wat::parse_str(out).expect("the printed text is valid wasm text")
    }

    /// The same module with the conversions left out, so that subtracting the
    /// two gives what this milestone added rather than what a module weighs.
    fn runtime_only_bytes(&self) -> Vec<u8> {
        let funcs = runtime::build(&self.rt);
        let mut out = String::from("(module\n  (memory 1 200)\n");
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
        out.push_str(")\n");
        wat::parse_str(out).expect("the printed text is valid wasm text")
    }

    fn instance(&self) -> tinyvm::WasmInstance {
        let bytes = self.bytes();
        let module = WasmModule::from_bytes_with(&bytes, Limits::default())
            .unwrap_or_else(|e| panic!("load gate rejected the module: {}", e.message()));
        module
            .instantiate()
            .unwrap_or_else(|e| panic!("instantiate failed: {}", e.message()))
    }
}

fn read_string(instance: &tinyvm::WasmInstance, ptr: i32) -> String {
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let header = &bytes[at..at + 4];
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("valid UTF-8")
}

/// One module that answers `__num_to_string(x)` for whatever `x` it is given,
/// built once and invoked many times -- assembling a module per value would
/// make the sweeps below unaffordable.
struct NumToString {
    instance: tinyvm::WasmInstance,
}

impl NumToString {
    fn new() -> Self {
        let mut prog = Prog::new(vec![ValType::F64], vec![ValType::I32]);
        prog.emit(&[Ins::LocalGet(0)]);
        prog.call(Cv::NumToString);
        let instance = prog.instance();
        NumToString { instance }
    }

    fn of(&mut self, x: f64) -> String {
        let out = self
            .instance
            .invoke_by_name("main", &[Val::F64(x)])
            .unwrap_or_else(|e| panic!("trap converting {x:?}: {}", e.message()));
        let Val::I32(ptr) = out[0] else {
            panic!("expected a pointer")
        };
        let s = read_string(&self.instance, ptr);
        self.instance
            .invoke_by_name("reset", &[])
            .expect("reset never traps");
        s
    }
}

/// The same shape for `__str_to_num`.
struct StrToNum {
    instance: tinyvm::WasmInstance,
    /// Where the literal being converted is written, and how much room it has.
    scratch: i32,
}

impl StrToNum {
    fn new() -> Self {
        // `main(ptr)` converts the string record at `ptr`, which the test
        // writes into guest memory itself. A data-segment literal would mean
        // one module per input.
        let mut prog = Prog::new(vec![ValType::I32], vec![ValType::F64]);
        prog.emit(&[Ins::LocalGet(0)]);
        prog.call(Cv::StrToNum);
        let scratch = prog.pool.heap_start();
        let instance = prog.instance();
        StrToNum { instance, scratch }
    }

    fn of(&mut self, s: &str) -> f64 {
        // The record goes well above the mark the bump pointer is reset to,
        // and one conversion allocates a few kilobytes at most, so the guest
        // cannot reach it.
        self.instance
            .invoke_by_name("reset", &[])
            .expect("reset never traps");
        let at = (self.scratch + 32768) as usize;
        {
            let mut view = self.instance.memory_mut().expect("guest memory");
            let mem: &mut [u8] = &mut view;
            mem[at..at + 4].copy_from_slice(&(s.len() as u32).to_le_bytes());
            mem[at + 4..at + 4 + s.len()].copy_from_slice(s.as_bytes());
        }
        let out = self
            .instance
            .invoke_by_name("main", &[Val::I32(at as i32)])
            .unwrap_or_else(|e| panic!("trap converting {s:?}: {}", e.message()));
        let Val::F64(x) = out[0] else {
            panic!("expected an f64")
        };
        x
    }
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
        Ins::CallIndirect(t, tab) => format!("call_indirect {tab} (type {t})"),
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
        Ins::I64Load(a, o) => format!("i64.load{}", memarg(*a, *o)),
        Ins::I64Store(a, o) => format!("i64.store{}", memarg(*a, *o)),
        Ins::MemorySize => "memory.size".into(),
        Ins::MemoryGrow => "memory.grow".into(),
        Ins::I32Const(v) => format!("i32.const {v}"),
        Ins::I64Const(v) => format!("i64.const {v}"),
        Ins::F64Const(v) => format!("f64.const {}", f64_wat(*v)),
        Ins::I32Eqz => "i32.eqz".into(),
        Ins::I32Eq => "i32.eq".into(),
        Ins::I32Ne => "i32.ne".into(),
        Ins::I32LtS => "i32.lt_s".into(),
        Ins::I32LtU => "i32.lt_u".into(),
        Ins::I32GeU => "i32.ge_u".into(),
        Ins::I32Add => "i32.add".into(),
        Ins::I32Sub => "i32.sub".into(),
        Ins::I32Mul => "i32.mul".into(),
        Ins::I32DivS => "i32.div_s".into(),
        Ins::I32RemS => "i32.rem_s".into(),
        Ins::I32And => "i32.and".into(),
        Ins::I32Or => "i32.or".into(),
        Ins::I32Shl => "i32.shl".into(),
        Ins::I32ShrU => "i32.shr_u".into(),
        Ins::I32Xor => "i32.xor".into(),
        Ins::I32ShrS => "i32.shr_s".into(),
        Ins::I64Eq => "i64.eq".into(),
        Ins::I64Add => "i64.add".into(),
        Ins::F64Eq => "f64.eq".into(),
        Ins::F64Ne => "f64.ne".into(),
        Ins::F64Lt => "f64.lt".into(),
        Ins::F64Gt => "f64.gt".into(),
        Ins::F64Le => "f64.le".into(),
        Ins::F64Ge => "f64.ge".into(),
        Ins::F64Abs => "f64.abs".into(),
        Ins::F64Neg => "f64.neg".into(),
        Ins::F64Ceil => "f64.ceil".into(),
        Ins::F64Floor => "f64.floor".into(),
        Ins::F64Nearest => "f64.nearest".into(),
        Ins::F64Sqrt => "f64.sqrt".into(),
        Ins::F64Min => "f64.min".into(),
        Ins::F64Max => "f64.max".into(),
        Ins::F64Add => "f64.add".into(),
        Ins::F64Sub => "f64.sub".into(),
        Ins::F64Mul => "f64.mul".into(),
        Ins::F64Div => "f64.div".into(),
        Ins::F64Copysign => "f64.copysign".into(),
        Ins::F64Trunc => "f64.trunc".into(),
        Ins::I32TruncF64S => "i32.trunc_f64_s".into(),
        Ins::I32WrapI64 => "i32.wrap_i64".into(),
        Ins::I64ExtendI32U => "i64.extend_i32_u".into(),
        Ins::F64ConvertI32S => "f64.convert_i32_s".into(),
        Ins::F64ConvertI32U => "f64.convert_i32_u".into(),
        Ins::F64ReinterpretI64 => "f64.reinterpret_i64".into(),
        Ins::I64ReinterpretF64 => "i64.reinterpret_f64".into(),
    }
}

// =========================================================================
// The spec, restated so the answers are checked against it and not against a
// second implementation of the same guess
// =========================================================================

/// The digits and the decimal exponent Rust's shortest-round-trip formatter
/// produces for a finite, non-zero `x`: `0.d1d2...dk * 10^n`.
fn rust_digits(x: f64) -> (Vec<u8>, i32) {
    let s = format!("{:e}", x.abs());
    let (m, e) = s.split_once('e').expect("LowerExp always has an e");
    let exp: i32 = e.parse().expect("a decimal exponent");
    let ds: Vec<u8> = m.bytes().filter(|b| *b != b'.').map(|b| b - b'0').collect();
    (ds, exp + 1)
}

/// The *exact* significant decimal digits of a finite, non-zero `x`, with the
/// same `n`. Every binary64 has a terminating decimal expansion, and Rust's
/// `{:.N}` is exact rather than approximate, so this is a fact about the
/// number and not another shortest-digit algorithm.
fn exact_digits(x: f64) -> (Vec<u8>, i32) {
    let s = format!("{:.1100}", x.abs());
    let (int, frac) = s.split_once('.').expect("a point at that precision");
    let all: Vec<u8> = int.bytes().chain(frac.bytes()).map(|b| b - b'0').collect();
    let lead = all.iter().position(|d| *d != 0).expect("non-zero");
    let mut ds = all[lead..].to_vec();
    while ds.last() == Some(&0) {
        ds.pop();
    }
    (ds, int.len() as i32 - lead as i32)
}

/// ECMA-262 6.1.6.1.20 step 5, as three separate claims about `(digits, n)`.
///
/// `k` and `n` come from Rust, which is a shortest-round-trip formatter and so
/// settles the length and the point. `s` itself is settled here: it must round
/// -trip, and where it differs from Rust's it must be an exact tie broken
/// towards the even `s`, which is the sentence Rust does not implement.
fn verify_step5(x: f64, ds: &[u8], n: i32) -> Result<(), String> {
    let k = ds.len() as i32;
    let text: String = ds.iter().map(|b| (b'0' + b) as char).collect();
    let lit = format!("{}e{}", text, n - k);
    let back: f64 = lit.parse().expect("a decimal literal");
    if back != x.abs() {
        return Err(format!("{lit} does not read back as {:?}", x.abs()));
    }
    let (rd, rn) = rust_digits(x);
    if rd.len() as i32 != k || rn != n {
        return Err(format!(
            "k/n disagree with the shortest oracle: {k}@{n} vs {}@{rn}",
            rd.len()
        ));
    }
    if rd == ds {
        return Ok(());
    }
    // An exact tie is the only licence to differ, and it is provable: the
    // number's exact expansion is these k digits with one more `5` on the end.
    let (ex, en) = exact_digits(x);
    let mut tie = ex.clone();
    let is_tie = en == n && ex.len() == k as usize + 1 && *ex.last().expect("non-empty") == 5;
    if !is_tie {
        return Err(format!(
            "differs from the oracle without a tie to justify it: {ds:?} vs {rd:?}"
        ));
    }
    tie.pop();
    // The two candidates are the truncation and its successor; exactly one is
    // even, and that is the one the spec names.
    let lo = tie.clone();
    let mut hi = tie;
    let mut c = 1u8;
    for d in hi.iter_mut().rev() {
        let t = *d + c;
        *d = t % 10;
        c = t / 10;
    }
    if c != 0 {
        return Err("the tie carried past the leading digit, which step 5's \
                    `s is not divisible by 10` note says cannot happen"
            .into());
    }
    let want = if lo.last().expect("non-empty") % 2 == 0 {
        lo
    } else {
        hi
    };
    if ds != want {
        return Err(format!("tie broken the wrong way: {ds:?}, want {want:?}"));
    }
    Ok(())
}

/// ECMA-262 6.1.6.1.20 steps 6 to 9, transcribed from the spec text. Rust's
/// formatter is no oracle for this half: it has no exponential form at all.
fn ecma_layout(ds: &[u8], n: i32) -> String {
    let k = ds.len() as i32;
    let s: String = ds.iter().map(|b| (b'0' + b) as char).collect();
    if k <= n && n <= 21 {
        return format!("{}{}", s, "0".repeat((n - k) as usize));
    }
    if 0 < n && n <= 21 {
        return format!("{}.{}", &s[..n as usize], &s[n as usize..]);
    }
    if -6 < n && n <= 0 {
        return format!("0.{}{}", "0".repeat((-n) as usize), s);
    }
    let e = n - 1;
    let sign = if e >= 0 { '+' } else { '-' };
    let mag = e.abs();
    if k == 1 {
        format!("{s}e{sign}{mag}")
    } else {
        format!("{}.{}e{sign}{mag}", &s[..1], &s[1..])
    }
}

/// The whole of 6.1.6.1.20, as the reference the emitted code is measured
/// against. Steps 1 to 4 first, then step 5's digits by way of `verify_step5`,
/// then the layout.
fn ecma_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x < 0.0 {
        return format!("-{}", ecma_to_string(-x));
    }
    if x.is_infinite() {
        return "Infinity".into();
    }
    let (mut ds, n) = rust_digits(x);
    // Apply the even-`s` rule the oracle does not have.
    let (ex, en) = exact_digits(x);
    let k = ds.len();
    if en == n && ex.len() == k + 1 && *ex.last().expect("non-empty") == 5 {
        let lo = &ex[..k];
        if lo[k - 1] % 2 == 0 {
            ds = lo.to_vec();
        } else {
            let mut hi = lo.to_vec();
            let mut c = 1u8;
            for d in hi.iter_mut().rev() {
                let t = *d + c;
                *d = t % 10;
                c = t / 10;
            }
            assert_eq!(c, 0, "a tie cannot carry past the leading digit");
            ds = hi;
        }
    }
    ecma_layout(&ds, n)
}

/// Invert the layout, so a produced string can be checked as `(digits, n)`.
fn parse_layout(s: &str) -> (Vec<u8>, i32) {
    let (mant, exp) = match s.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().expect("a decimal exponent") + 1),
        None => (s, 0),
    };
    let (int, frac) = match mant.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mant, ""),
    };
    let all: Vec<u8> = int.bytes().chain(frac.bytes()).map(|b| b - b'0').collect();
    let n = if s.contains('e') {
        exp
    } else {
        int.len() as i32
    };
    let lead = all.iter().position(|d| *d != 0).unwrap_or(0);
    let mut ds = all[lead..].to_vec();
    let n = n - lead as i32;
    while ds.last() == Some(&0) {
        ds.pop();
    }
    (ds, n)
}

// =========================================================================
// Number::toString
// =========================================================================

#[test]
fn the_four_answers_before_step_five() {
    let mut f = NumToString::new();
    assert_eq!(f.of(f64::NAN), "NaN");
    assert_eq!(f.of(0.0), "0");
    // -0 is a distinct Number and its String is still "0": step 2, not step 3.
    assert_eq!(f.of(-0.0), "0");
    assert_eq!(f.of(f64::INFINITY), "Infinity");
    assert_eq!(f.of(f64::NEG_INFINITY), "-Infinity");
}

#[test]
fn the_layout_thresholds_are_the_spec_numbers_and_not_a_formatters() {
    let mut f = NumToString::new();
    // Step 6 runs out at n == 21; step 9 takes over at 22.
    assert_eq!(f.of(1e20), "100000000000000000000");
    assert_eq!(f.of(1e21), "1e+21");
    // Step 8 runs out at n == -6; step 9 takes over below.
    assert_eq!(f.of(1e-6), "0.000001");
    assert_eq!(f.of(1e-7), "1e-7");
    // Both are places Rust's own formatter answers differently.
    assert_eq!(format!("{}", 1e21f64), "1000000000000000000000");
    assert_eq!(format!("{}", 1e-7f64), "0.0000001");
}

#[test]
fn the_sentences_the_readme_promises() {
    let mut f = NumToString::new();
    assert_eq!(f.of(0.1), "0.1");
    assert_eq!(f.of(0.1 + 0.2), "0.30000000000000004");
    assert_eq!(f.of(5e-324), "5e-324");
    assert_eq!(f.of(f64::MAX), "1.7976931348623157e+308");
    assert_eq!(f.of(f64::MIN_POSITIVE), "2.2250738585072014e-308");
    assert_eq!(f.of(1.5), "1.5");
    assert_eq!(f.of(-1.5), "-1.5");
    assert_eq!(f.of(1.0), "1");
    assert_eq!(f.of(-7.0), "-7");
    assert_eq!(f.of(1234.0), "1234");
    // The exact integer stops being the answer above 2^53: this double is
    // 1152921504606846976, and the shortest decimal that reads back as it is
    // not that number.
    assert_eq!(f.of(2f64.powi(60)), "1152921504606847000");
    // Step 3 is a prefix on the whole of the rest, exponential form included.
    assert_eq!(f.of(-1e21), "-1e+21");
    assert_eq!(f.of(-5e-324), "-5e-324");
    assert_eq!(f.of(-1e-7), "-1e-7");
    assert_eq!(f.of(-0.000001), "-0.000001");
    // A one-digit `s` takes step 9's `k == 1` arm, with no point in it.
    assert_eq!(f.of(1e22), "1e+22");
    assert_eq!(f.of(1e-8), "1e-8");
    // Three-, two- and one-digit exponents each take their own arm, and the
    // middle digit of a three-digit one is not optional.
    assert_eq!(f.of(1e100), "1e+100");
    assert_eq!(f.of(1e-100), "1e-100");
    assert_eq!(f.of(1e308), "1e+308");
    assert_eq!(f.of(1e-308), "1e-308");
    assert_eq!(f.of(1e30), "1e+30");
}

#[test]
fn an_exact_tie_is_broken_towards_the_even_s() {
    let mut f = NumToString::new();
    // 785068460487425.25 exactly: the two 16-digit candidates ...25.2 and
    // ...25.3 are equidistant and both read back as it. Step 5's last
    // sentence picks the even `s`. Rust's formatter picks the other one.
    let x = f64::from_bits(0x4306_501f_f5b1_980a);
    assert_eq!(x, 785068460487425.2);
    assert_eq!(f.of(x), "785068460487425.2");
    assert_eq!(format!("{x}"), "785068460487425.3");
    for bits in [
        0xc30a_a61f_a224_75cau64,
        0x42d1_1c37_8bee_3b08,
        0x431a_79c6_d44c_a959,
    ] {
        let x = f64::from_bits(bits);
        assert_eq!(f.of(x), ecma_to_string(x), "for {x:?}");
    }
}

/// One sweep helper: convert, then hold the answer to the spec.
fn sweep(f: &mut NumToString, xs: impl Iterator<Item = f64>, tag: &str) -> usize {
    let mut checked = 0;
    for x in xs {
        if !x.is_finite() || x == 0.0 {
            continue;
        }
        let got = f.of(x);
        let (ds, n) = parse_layout(got.strip_prefix('-').unwrap_or(&got));
        if let Err(why) = verify_step5(x, &ds, n) {
            panic!(
                "{tag}: {x:?} (bits {:#x}) produced {got:?}: {why}",
                x.to_bits()
            );
        }
        let want = if x < 0.0 {
            format!("-{}", ecma_layout(&ds, n))
        } else {
            ecma_layout(&ds, n)
        };
        assert_eq!(got, want, "{tag}: layout for {x:?}");
        checked += 1;
    }
    checked
}

#[test]
fn integers_convert_exactly() {
    let mut f = NumToString::new();
    for i in 0..800u32 {
        assert_eq!(f.of(f64::from(i)), i.to_string());
        assert_eq!(
            f.of(-f64::from(i)),
            if i == 0 { "0".into() } else { format!("-{i}") }
        );
    }
    // Every integer below 2^53 is exactly representable and no shorter decimal
    // lies inside its rounding interval, so the answer is its own digits.
    for p in 0..53 {
        let x = 2f64.powi(p);
        if x < 1e21 {
            assert_eq!(f.of(x), format!("{}", x as u64));
        }
    }
    let n = sweep(&mut f, (0..1500u32).map(|i| f64::from(i) * 7.0), "integers");
    assert!(n > 1400);
}

#[test]
fn subnormals_and_the_extremes_convert() {
    let mut f = NumToString::new();
    let n = sweep(&mut f, (1..400u64).map(f64::from_bits), "subnormal");
    assert!(n > 390);
    sweep(
        &mut f,
        (0..150u64).map(|i| f64::from_bits(0x7FEF_FFFF_FFFF_FFFF - i)),
        "near max",
    );
    sweep(
        &mut f,
        (0..150u64).map(|i| f64::from_bits(0x0010_0000_0000_0000 + i)),
        "smallest normal",
    );
}

#[test]
fn either_side_of_the_two_layout_thresholds() {
    let mut f = NumToString::new();
    for (centre, tag) in [
        (1e21f64, "1e21"),
        (1e-7f64, "1e-7"),
        (1e-6, "1e-6"),
        (1e20, "1e20"),
    ] {
        sweep(
            &mut f,
            (0..150u64).flat_map(move |i| {
                [
                    f64::from_bits(centre.to_bits() + i),
                    f64::from_bits(centre.to_bits() - i),
                ]
            }),
            tag,
        );
    }
}

#[test]
fn a_fixed_seed_sample_of_the_whole_domain() {
    let mut f = NumToString::new();
    let mut st = 0x243F_6A88_85A3_08D3u64;
    let bits = std::iter::from_fn(move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        Some(st)
    });
    // The seed is fixed so a failure reproduces; the count is what a debug
    // build of the interpreter can carry in a test run.
    let n = sweep(&mut f, bits.take(1200).map(f64::from_bits), "random bits");
    assert!(n > 900, "only {n} of the sample were finite and non-zero");
}

#[test]
fn a_fixed_seed_sample_of_ordinary_decimals() {
    let mut f = NumToString::new();
    let mut st = 0x9E37_7979_7F4A_7C15u64;
    let xs = std::iter::from_fn(move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        let mant = (st % 10_000_000_000_000_000) as f64;
        let e = (st >> 40) % 60;
        Some(mant / 10f64.powi(e as i32))
    });
    sweep(&mut f, xs.take(1500), "random decimals");
}

// =========================================================================
// StringToNumber
// =========================================================================

#[test]
fn the_empty_and_whitespace_only_strings_are_plus_zero() {
    let mut f = StrToNum::new();
    for s in ["", " ", "\t\n\r ", "\u{00a0}\u{feff}\u{2028}\u{3000}"] {
        let x = f.of(s);
        assert_eq!(x, 0.0, "for {s:?}");
        assert!(x.is_sign_positive(), "{s:?} must be +0, not -0");
    }
}

#[test]
fn the_grammar_the_spec_writes() {
    let mut f = StrToNum::new();
    for (s, want) in [
        ("0", 0.0),
        ("1", 1.0),
        ("  42  ", 42.0),
        ("+42", 42.0),
        ("-42", -42.0),
        ("1.5", 1.5),
        (".5", 0.5),
        ("5.", 5.0),
        ("1e3", 1000.0),
        ("1E3", 1000.0),
        ("1e+3", 1000.0),
        ("1e-3", 0.001),
        ("0.1", 0.1),
        ("Infinity", f64::INFINITY),
        ("-Infinity", f64::NEG_INFINITY),
        ("+Infinity", f64::INFINITY),
        ("  Infinity  ", f64::INFINITY),
        ("0x10", 16.0),
        ("0X1f", 31.0),
        ("0o17", 15.0),
        ("0b101", 5.0),
        ("0xffffffffffffffff", 18446744073709551615.0),
        ("1e400", f64::INFINITY),
        ("1e-400", 0.0),
    ] {
        assert_eq!(f.of(s), want, "for {s:?}");
    }
    assert!(f.of("-0").is_sign_negative(), "-0 keeps its sign");
    assert_eq!(f.of("-0"), 0.0);
}

#[test]
fn a_string_the_grammar_does_not_accept_is_nan() {
    let mut f = StrToNum::new();
    for s in [
        "x", "1x", ".", "+", "-", "1e", "1e+", "e3", "0x", "0b", "0o", "1 2", "--1", "1.2.3",
        "infinity", "INFINITY", "Infinit", "0x1g", "1_000", "+0x10", "0b2", "0o8", " 1 . 5 ",
    ] {
        assert!(f.of(s).is_nan(), "{s:?} should be NaN, got {}", f.of(s));
    }
}

#[test]
fn conversion_is_correctly_rounded_and_not_an_accumulation_of_roundings() {
    let mut f = StrToNum::new();
    // The naive `d * 10 + digit` then `/ 10^k` answers a neighbouring double
    // for several of these; Rust's own parser is correctly rounded, so it is
    // the oracle here and there is no threshold for it to disagree at.
    for s in [
        "0.1",
        "0.2",
        "0.3",
        "2.2250738585072011e-308",
        "2.2250738585072012e-308",
        "1.7976931348623157e308",
        "4.9406564584124654e-324",
        "2.4703282292062327e-324",
        "2.4703282292062328e-324",
        "9007199254740993",
        "9007199254740992.5",
        "1e23",
        "8.98846567431158e307",
        "123456789012345678901234567890",
        "0.000000000000000000000000001",
        "5e-324",
        "1.0000000000000002",
    ] {
        let want: f64 = s.parse().expect("Rust parses it");
        let got = f.of(s);
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "for {s:?}: got {got:?}, want {want:?}"
        );
    }
}

#[test]
fn the_long_digit_string_path_agrees_with_rust() {
    let mut f = StrToNum::new();
    // Past 768 significant digits the tail becomes one sticky bit; these are
    // the shapes where that decision is visible.
    let mut cases = vec![
        format!("0.{}1", "0".repeat(320)),
        format!("{}", "9".repeat(400)),
        format!("1.{}", "0".repeat(900)),
        format!("1.{}1", "0".repeat(900)),
        format!("{}e-320", "1".repeat(800)),
    ];
    // The midpoint between 1 and its successor, written out exactly and then
    // with one more digit on the end -- the tie and the nudge past it.
    cases.push("1.00000000000000011102230246251565404236316680908203125".into());
    cases.push("1.000000000000000111022302462515654042363166809082031251".into());
    for s in cases {
        let want: f64 = s.parse().expect("Rust parses it");
        let got = f.of(&s);
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "for {}...: got {got:?}, want {want:?}",
            &s[..s.len().min(40)]
        );
    }
}

#[test]
fn string_to_number_round_trips_number_to_string() {
    // The two halves are each other's inverse on every finite Number: that is
    // what "reads back as the same binary64" in step 5 means, checked through
    // this engine's own reader rather than Rust's.
    let mut to_s = NumToString::new();
    let mut to_n = StrToNum::new();
    let mut st = 0x0DDB_1A5E_5BAD_5EEDu64;
    let mut n = 0;
    for _ in 0..600 {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        let x = f64::from_bits(st);
        if !x.is_finite() || x == 0.0 {
            continue;
        }
        let text = to_s.of(x);
        let back = to_n.of(&text);
        assert_eq!(back.to_bits(), x.to_bits(), "{x:?} -> {text:?} -> {back:?}");
        n += 1;
    }
    assert!(n > 400);
}

// =========================================================================
// String relational comparison
// =========================================================================

/// `__str_cmp` over two strings the host writes into guest memory, so a sweep
/// costs one invocation rather than one module.
struct StrCmp {
    instance: tinyvm::WasmInstance,
    scratch: i32,
}

impl StrCmp {
    fn new() -> Self {
        let mut prog = Prog::new(vec![ValType::I32, ValType::I32], vec![ValType::I32]);
        prog.emit(&[Ins::LocalGet(0), Ins::LocalGet(1)]);
        prog.call(Cv::StrCmp);
        let scratch = prog.pool.heap_start();
        StrCmp {
            instance: prog.instance(),
            scratch,
        }
    }

    fn of(&mut self, a: &str, b: &str) -> i32 {
        let pa = (self.scratch + 16384) as usize;
        let pb = pa + 8192;
        {
            let mut view = self.instance.memory_mut().expect("guest memory");
            let mem: &mut [u8] = &mut view;
            for (at, s) in [(pa, a), (pb, b)] {
                mem[at..at + 4].copy_from_slice(&(s.len() as u32).to_le_bytes());
                mem[at + 4..at + 4 + s.len()].copy_from_slice(s.as_bytes());
            }
        }
        let out = self
            .instance
            .invoke_by_name("main", &[Val::I32(pa as i32), Val::I32(pb as i32)])
            .unwrap_or_else(|e| panic!("trap comparing {a:?} and {b:?}: {}", e.message()));
        match out[0] {
            Val::I32(v) => v,
            _ => panic!("expected an i32"),
        }
    }
}

fn cmp(a: &str, b: &str) -> i32 {
    StrCmp::new().of(a, b)
}

/// The spec's own definition: compare the UTF-16 code units.
fn ecma_cmp(a: &str, b: &str) -> i32 {
    let ua: Vec<u16> = a.encode_utf16().collect();
    let ub: Vec<u16> = b.encode_utf16().collect();
    match ua.cmp(&ub) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

#[test]
fn comparison_is_by_code_unit() {
    for (a, b) in [
        ("", ""),
        ("", "a"),
        ("a", ""),
        ("a", "b"),
        ("b", "a"),
        ("a", "ab"),
        ("ab", "a"),
        ("abc", "abd"),
        ("Z", "a"),
        ("\u{00e9}", "\u{00e8}"),
        ("\u{4e00}", "\u{4e01}"),
    ] {
        assert_eq!(cmp(a, b), ecma_cmp(a, b), "comparing {a:?} and {b:?}");
    }
}

#[test]
fn a_supplementary_character_sorts_as_its_surrogates_and_not_as_its_code_point() {
    // This is the whole reason the comparison decodes. U+10000 is above U+E000
    // as a code point and therefore above it in UTF-8 byte order, but its
    // first UTF-16 code unit is U+D800, which is below.
    let sup = "\u{10000}";
    let bmp = "\u{e000}";
    assert!(sup.as_bytes() > bmp.as_bytes(), "byte order says greater");
    assert_eq!(ecma_cmp(sup, bmp), -1, "code-unit order says less");
    assert_eq!(cmp(sup, bmp), -1);
    assert_eq!(cmp(bmp, sup), 1);
    for (a, b) in [
        ("\u{10000}", "\u{10001}"),
        ("\u{10ffff}", "\u{ffff}"),
        ("a\u{10000}", "a\u{e000}"),
        ("\u{d7ff}", "\u{10000}"),
    ] {
        assert_eq!(cmp(a, b), ecma_cmp(a, b), "comparing {a:?} and {b:?}");
    }
}

// =========================================================================
// What it costs
// =========================================================================

#[test]
fn the_emitted_size_of_each_conversion_is_written_down() {
    // Not a budget -- a record, so a change in it is visible in a diff rather
    // than discovered later. The number is instructions, because the bytes
    // depend on an encoder in another lane's file.
    let mut pool = StringPool::default();
    let names = convert::Names::intern(&mut pool);
    let ctx = convert::Ctx {
        func_base: runtime::SET.len() as u32,
        runtime_base: 0,
        names,
    };
    let funcs = convert::build(&ctx);
    let total: usize = funcs.iter().map(|f| f.body.len()).sum();
    let by = |name: &str| {
        funcs
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.body.len())
            .unwrap_or_else(|| panic!("{name} is in the set"))
    };
    let bignum: usize = funcs
        .iter()
        .filter(|f| f.name.starts_with("__bn_"))
        .map(|f| f.body.len())
        .sum();
    println!("convert: {} functions, {total} instructions", funcs.len());
    println!("  bignum          {bignum}");
    println!("  __dragon4       {}", by("__dragon4"));
    println!("  __num_to_string {}", by("__num_to_string"));
    println!("  __ws_len        {}", by("__ws_len"));
    println!("  __skip_ws       {}", by("__skip_ws"));
    println!("  __ratio_to_f64  {}", by("__ratio_to_f64"));
    println!("  __digits_to_f64 {}", by("__digits_to_f64"));
    println!("  __str_to_num    {}", by("__str_to_num"));
    println!("  __u16_next      {}", by("__u16_next"));
    println!("  __str_cmp       {}", by("__str_cmp"));

    // And in bytes, which is the number the milestone is judged on. Measured
    // by assembling the same module with and without the conversions -- the
    // reference assembler rather than this compiler's encoder, because
    // `encode.rs` is another lane's file and does not carry these
    // instructions yet, so treat it as a close proxy and not a promise.
    let prog = Prog::new(vec![], vec![]);
    let with = prog.bytes().len();
    let without = prog.runtime_only_bytes().len();
    println!("module with the conversions:    {with} bytes");
    println!("module without them:            {without} bytes");
    println!("the conversions cost:           {} bytes", with - without);
    // Per function, by assembling the module again with that one body
    // replaced by a single `unreachable` and taking the difference. Exact,
    // and it needs no separate module per function to keep the calls valid.
    let mut group = std::collections::BTreeMap::<&str, usize>::new();
    for cv in convert::SET {
        let n = prog.bytes_without(*cv).len();
        let cost = with - n;
        let name = cv.symbol();
        let key = if name.starts_with("__bn_") {
            "bignum"
        } else {
            name
        };
        *group.entry(key).or_default() += cost;
    }
    for (name, bytes) in &group {
        println!("  {name:<16} {bytes} bytes");
    }
    assert_eq!(funcs.len(), convert::SET.len());
}

// =========================================================================
// A stack checker, because the load gate's refusal has no address in it
// =========================================================================

/// Walk one function's IR keeping the operand-stack height, and report the
/// first instruction that would underflow or the first region that ends at the
/// wrong height.
///
/// tinyvm's gate says "operand stack underflow" and nothing else, which is the
/// right answer for a host and useless for finding the bug. This is the same
/// arithmetic done where the instruction index is still in hand.
fn stack_trace(f: &RtFunc, sigs: &[(usize, usize)]) -> Result<(), String> {
    // (pops, pushes) for everything that is not control flow or a call.
    fn effect(ins: &Ins) -> (usize, usize) {
        use Ins::*;
        match ins {
            LocalGet(_) | GlobalGet(_) | MemorySize | I32Const(_) | I64Const(_) | F64Const(_) => {
                (0, 1)
            }
            LocalSet(_) | GlobalSet(_) | Drop => (1, 0),
            LocalTee(_) => (1, 1),
            I32Load(..) | I32Load8U(..) | I64Load(..) | MemoryGrow => (1, 1),
            I32Store(..) | I32Store8(..) | I64Store(..) => (2, 0),
            I32Eqz | F64Abs | F64Neg | F64Trunc | I32TruncF64S | I32WrapI64 | I64ExtendI32U
            | F64ConvertI32S | F64ReinterpretI64 | I64ReinterpretF64 => (1, 1),
            _ => (2, 1),
        }
    }
    let mut height: isize = 0;
    // Each open region remembers the height outside it and whether it is a
    // function-level frame.
    let mut regions: Vec<isize> = vec![0];
    let mut unreachable = false;
    for (at, ins) in f.body.iter().enumerate() {
        let before = height;
        match ins {
            Ins::Block(_) | Ins::Loop(_) => regions.push(height),
            Ins::If(_) => {
                height -= 1;
                regions.push(height);
            }
            Ins::End => {
                let base = regions.pop().ok_or("more ends than blocks")?;
                if !unreachable && height != base {
                    return Err(format!(
                        "instruction {at}: a block that started at height {base} ends at {height}"
                    ));
                }
                height = base;
                unreachable = false;
            }
            Ins::Br(_) | Ins::Return | Ins::Unreachable => unreachable = true,
            Ins::BrIf(_) => height -= 1,
            Ins::Call(i) => {
                let (p, r) = sigs
                    .get(*i as usize)
                    .copied()
                    .ok_or_else(|| format!("instruction {at}: call {i} has no callee"))?;
                height -= p as isize;
                height += r as isize;
            }
            other => {
                let (p, r) = effect(other);
                height -= p as isize;
                height += r as isize;
            }
        }
        let base = *regions.last().unwrap_or(&0);
        // The floor is checked against what the instruction *popped*, not its
        // net effect. A block may not reach below the height it started at,
        // and "push a value, then add to it inside an `if`" dips below that
        // floor while ending level -- which a net-effect check waves through
        // and the load gate does not.
        let popped = match ins {
            Ins::Block(_)
            | Ins::Loop(_)
            | Ins::End
            | Ins::Br(_)
            | Ins::Return
            | Ins::Unreachable => before,
            Ins::If(_) | Ins::BrIf(_) => before - 1,
            Ins::Call(i) => before - sigs[*i as usize].0 as isize,
            other => before - effect(other).0 as isize,
        };
        if !unreachable && (height < base || popped < base) {
            return Err(format!(
                "instruction {at} ({ins:?}) reaches below its block: height {before} -> {height}, \
                 low water {popped}, floor {base}"
            ));
        }
    }
    if !unreachable && height != f.results.len() as isize {
        return Err(format!(
            "ends at height {height} with {} result(s) declared",
            f.results.len()
        ));
    }
    Ok(())
}

#[test]
fn every_emitted_function_is_stack_balanced() {
    let prog = Prog::new(vec![], vec![]);
    let funcs = prog.funcs();
    let sigs: Vec<(usize, usize)> = funcs
        .iter()
        .map(|f| (f.params.len(), f.results.len()))
        .collect();
    let mut bad = Vec::new();
    for f in &funcs {
        if let Err(why) = stack_trace(f, &sigs) {
            bad.push(format!("{}: {why}", f.name));
        }
    }
    assert!(bad.is_empty(), "unbalanced:\n{}", bad.join("\n"));
}

/// The wide sweep, kept out of the ordinary run because it is minutes rather
/// than seconds -- one conversion of a subnormal is about 80 000 interpreted
/// wasm instructions, and this is a quarter of a million of them.
///
/// This is the evidence run, not a smoke test. Re-run it after any change to
/// `__dragon4`:
///
/// ```sh
/// cargo test -p tinyvm-qjs --test conversions --release -- --ignored --nocapture
/// ```
#[test]
#[ignore = "minutes; the evidence run, not the smoke test"]
fn the_wide_sweep() {
    let mut f = NumToString::new();
    let mut total = 0;
    total += sweep(&mut f, (0..30000u32).map(f64::from), "integers");
    total += sweep(&mut f, (1..8000u64).map(f64::from_bits), "subnormals");
    total += sweep(
        &mut f,
        (-330i32..=308).map(|p| format!("1e{p}").parse::<f64>().expect("a literal")),
        "powers of ten",
    );
    total += sweep(
        &mut f,
        (0..8000u64).map(|i| f64::from_bits(0x7FEF_FFFF_FFFF_FFFF - i)),
        "near max",
    );
    for centre in [1e21f64, 1e-7, 1e-6, 1e20, 1.0, 0.1] {
        total += sweep(
            &mut f,
            (0..4000u64).flat_map(move |i| {
                [
                    f64::from_bits(centre.to_bits() + i),
                    f64::from_bits(centre.to_bits() - i),
                ]
            }),
            "threshold",
        );
    }
    let mut st = 0x243F_6A88_85A3_08D3u64;
    let bits = std::iter::from_fn(move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        Some(st)
    });
    total += sweep(
        &mut f,
        bits.take(120_000).map(f64::from_bits),
        "random bits",
    );
    let mut st = 0x9E37_7979_7F4A_7C15u64;
    let xs = std::iter::from_fn(move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        let mant = (st % 10_000_000_000_000_000) as f64;
        let e = (st >> 40) % 60;
        Some(mant / 10f64.powi(e as i32))
    });
    total += sweep(&mut f, xs.take(60000), "random decimals");
    println!("the wide sweep held {total} values to ECMA-262 6.1.6.1.20");
    assert!(total > 250_000, "only {total} values were checked");
}

#[test]
fn a_fixed_seed_sample_of_string_pairs() {
    // The alphabet is chosen to put the disagreement in reach: a supplementary
    // character next to the BMP range whose code units sit between the two
    // surrogate halves, plus a lone surrogate's neighbours.
    let alphabet: Vec<char> =
        "ab\u{7f}\u{80}\u{7ff}\u{800}\u{d7ff}\u{e000}\u{fffd}\u{ffff}\u{10000}\u{10001}\u{10ffff}"
            .chars()
            .collect();
    let mut c = StrCmp::new();
    let mut st = 0xB16B_00B5_1234_5678u64;
    let mut next = move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        st
    };
    for _ in 0..600 {
        let make = |r: u64| -> String {
            let n = (r % 5) as usize;
            (0..n)
                .map(|k| alphabet[((r >> (8 * k + 3)) as usize) % alphabet.len()])
                .collect()
        };
        let a = make(next());
        let b = make(next());
        assert_eq!(c.of(&a, &b), ecma_cmp(&a, &b), "comparing {a:?} and {b:?}");
    }
    // And every ordered pair of single characters from the alphabet, which is
    // where the surrogate reordering actually lives.
    for x in &alphabet {
        for y in &alphabet {
            let (a, b) = (x.to_string(), y.to_string());
            assert_eq!(c.of(&a, &b), ecma_cmp(&a, &b), "comparing {a:?} and {b:?}");
        }
    }
}

#[test]
fn a_fixed_seed_sample_of_numeric_literals() {
    // Rust's `str::parse::<f64>` is correctly rounded, so it is a complete
    // oracle here -- unlike the formatter, it has no threshold and no tie rule
    // to disagree about.
    let mut f = StrToNum::new();
    let mut st = 0xFEED_FACE_CAFE_BEEFu64;
    let mut next = move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        st
    };
    let mut checked = 0;
    for _ in 0..1200 {
        let r = next();
        let digits = (r % 19) as usize + 1;
        let mant: String = (0..digits)
            .map(|k| char::from(b'0' + ((r >> (3 * k + 5)) % 10) as u8))
            .collect();
        let point = (next() % (digits as u64 + 1)) as usize;
        let exp = (next() % 60) as i64 - 30;
        let sign = if next() % 2 == 0 { "" } else { "-" };
        let body = format!("{}.{}", &mant[..point], &mant[point..]);
        for s in [
            format!("{sign}{body}"),
            format!("{sign}{body}e{exp}"),
            format!("  {sign}{body}e{exp}  "),
        ] {
            let want: f64 = s.trim().parse().expect("Rust parses it");
            let got = f.of(&s);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "for {s:?}: got {got:?}, want {want:?}"
            );
            checked += 1;
        }
    }
    // The wide exponents, where the fast path gives out and the exact one runs.
    for _ in 0..400 {
        let r = next();
        let digits = (r % 17) as usize + 1;
        let mant: String = (0..digits)
            .map(|k| char::from(b'0' + ((r >> (3 * k + 5)) % 10) as u8))
            .collect();
        let exp = (next() % 700) as i64 - 350;
        let s = format!("{mant}e{exp}");
        let want: f64 = s.parse().expect("Rust parses it");
        let got = f.of(&s);
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "for {s:?}: got {got:?}, want {want:?}"
        );
        checked += 1;
    }
    assert!(checked >= 4000, "only {checked} literals were checked");
}
