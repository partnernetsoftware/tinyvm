//! Attacks on the two things this milestone added: hand-rolled unwinding, and
//! `JSON.parse`/`JSON.stringify`.
//!
//! This file breaks nothing on purpose. Every row is a minimal reproducer that
//! was **run**; where the engine's answer was not the answer ECMA-262 gives,
//! the engine's answer was asserted as observed and marked `DEFECT (open):`
//! with the right answer beside it, so the debt was visible in a green suite
//! and the assertion inverted loudly on the day it was paid.
//!
//! All five have been paid, and each of those rows now asserts the
//! specification's answer instead -- the stale in-flight flag, the finalizer
//! that overwrote the completion value, the fault code `guest_fault` could not
//! read, the channel `JSON` needs and did not declare, and `JSON` itself being
//! unreachable from a script. The tests kept their evidence and inverted their
//! claim; none of them was relaxed.
//!
//! # Where the attacks came from
//!
//! Unwinding is encoded by the compiler because tinyvm's core has no
//! exception instruction (`crates/tinyvm/src/wasm.rs:2931` has no arm for
//! `try` 0x06 / `catch` 0x07 / `throw` 0x08 / `try_table` 0x1F, and
//! `crates/tinyvm/src/wasm.rs:4852` refuses the tag section id 13). A flag in
//! a module global plus a check after every call is the design; the two
//! questions that design has to answer are **can the flag be wrong** and
//! **can the branch go to the wrong label**, and this file asks both of them
//! from outside the compiler.
//!
//! The label question came back clean over 50 shapes. The flag question did
//! not: a module global outlives a call, and nothing put it back. Two
//! instructions in the entry prologue do now, and
//! `a_handled_throw_is_never_a_throw_the_previous_call_raised` is where that
//! is held.

// The harness below is `tests/json.rs`'s, copied rather than shared because
// an integration test cannot import another one. Parts of it (`stringify`,
// `wide`, the byte count) are used there and not here.
#![allow(dead_code)]

// Three source modules included by path. Each is used only in part here --
// the runtime's operators and the conversions' bignum have their own files --
// so the dead-code allowance is about this reader and not about the module.
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

use convert::{Cv, Js};
use repr::{
    BlockType, Ins, TAG_OBJECT, TAG_STRING, ValType, box_object, const_undefined, unbox_function,
};
use runtime::{
    ALIGN_WORD, Conversions, Ctx as RtCtx, FN_ELEMENT, FnBuild, PrimNames, RtFunc, StringPool,
};
use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

// =========================================================================
// Harness
// =========================================================================

/// Where the test writes a string record it wants the guest to read. Well
/// above anything one call allocates, and the `reset` export puts the bump
/// pointer back before every call, so the guest never reaches it.
const SCRATCH: i32 = 2_000_000;
/// Eight pages, so [`SCRATCH`] is addressable without the guest having to
/// grow memory first.
const PAGES: u32 = 40;

/// The one module every test in this file runs against.
///
/// It carries the runtime, the conversions and the JSON set, a table of two
/// adapters, and four exports that reach `JSON` the way a script would.
struct Engine {
    instance: tinyvm::WasmInstance,
    bytes: usize,
}

/// The three function sets, placed the way `emit::m1::lower` places them.
struct Bases {
    array_names: array::Names,
    pool: StringPool,
    rt: RtCtx,
    cv: convert::Ctx,
    js: convert::JsonCtx,
    unwind: Option<convert::Throwing>,
}

/// The three unwind globals sit after global 0, exactly as `emit` places them
/// after the heap pointer and the binding globals -- there are no bindings in
/// this harness, so they land at 1, 2 and 3.
const UNWIND: convert::Throwing = convert::Throwing {
    flag: 1,
    tag: 2,
    payload: 3,
};

impl Bases {
    fn new(unwind: Option<convert::Throwing>) -> Self {
        let mut pool = StringPool::default();
        let prim_names = PrimNames::intern(&mut pool);
        let cv_names = convert::Names::intern(&mut pool);
        let js_names = convert::JsonNames::intern(&mut pool);
        let array_names = array::Names::intern(&mut pool);
        let convert_base = runtime::SET.len() as u32;
        let json_base = convert_base + convert::SET.len() as u32;
        let rt = RtCtx {
            func_base: 0,
            heap_global: 0,
            type_names: None,
            prim_names,
            conversions: Conversions {
                num_to_string: convert_base + Cv::NumToString.offset(),
                str_to_num: convert_base + Cv::StrToNum.offset(),
                str_cmp: convert_base + Cv::StrCmp.offset(),
            },
            // Built directly rather than through the lowering, so this picks
            // the gate a program with no array would have picked.
            arrays: false,
            // These build the runtime directly; no program, so nothing captures.
            captures: false,
            string_length: None,
        };
        let cv = convert::Ctx {
            func_base: convert_base,
            runtime_base: 0,
            names: cv_names,
        };
        let js = convert::JsonCtx {
            func_base: json_base,
            runtime_base: 0,
            convert_base,
            unwind,
            arrays: json_base + convert::JSON_SET.len() as u32,
            names: js_names,
        };
        Bases {
            array_names,
            pool,
            rt,
            cv,
            js,
            unwind,
        }
    }

    fn funcs(&self) -> Vec<RtFunc> {
        let mut all = runtime::build(&self.rt);
        all.extend(convert::build(&self.cv));
        all.extend(convert::build_json(&self.js));
        // Not optional: `JSON.parse` calls `__arr_new`/`__arr_push` and
        // `__json_ser` dispatches to `__json_ser_arr`. Leaving it out builds a
        // module whose JSON functions call whatever sits at those indices,
        // which the load gate answers with "validation: type mismatch".
        all.extend(array::build(&array::Ctx {
            func_base: self.js.arrays,
            runtime_base: self.rt.func_base,
            names: self.array_names,
        }));
        all
    }
}

/// The uniform signature every call through a function value speaks here:
/// three JS values in, one out. Three because `JSON.stringify` declares
/// `(value, replacer, space)` -- see the note at [`Js::Stringify`].
fn uniform() -> (Vec<ValType>, Vec<ValType>) {
    (
        (0..3).flat_map(|_| repr::SLOTS).collect(),
        repr::SLOTS.to_vec(),
    )
}

/// One exported entry point: reach a method off the `JSON` namespace object
/// and call it through its value, with `argc` JS values already in the
/// entry's own parameters followed by `undefined` up to the uniform arity.
fn entry(js: &convert::JsonCtx, key: i32, argc: u32) -> FnBuild {
    let mut f = FnBuild::new(argc * repr::WIDTH);
    let ns = f.local(ValType::I32);
    let fv = f.value_local();

    // `const JSON = <the namespace object>` -- one call, exactly what the
    // lowering of the binding would emit.
    f.body.push(Ins::I32Const(ELEM_STRINGIFY));
    f.body.push(Ins::I32Const(ELEM_PARSE));
    f.body.push(js.call(Js::Ns));
    f.body.push(Ins::LocalSet(ns));

    // `JSON.stringify` / `JSON.parse` -- an ordinary property read.
    box_object(&[Ins::LocalGet(ns)], &mut f.body);
    f.body.push(Ins::I32Const(key));
    f.body.push(Ins::Call(runtime::Rt::ObjGet.offset()));
    f.body.push(Ins::LocalSet(fv + 1));
    f.body.push(Ins::LocalSet(fv));

    // The arguments, then the callee's element index.
    for i in 0..argc * repr::WIDTH {
        f.body.push(Ins::LocalGet(i));
    }
    for _ in argc..3 {
        const_undefined(&mut f.body);
    }
    unbox_function(fv, &mut f.body);
    f.body.push(Ins::I32Load(ALIGN_WORD, FN_ELEMENT));
    f.body.push(Ins::CallIndirect(UNIFORM_TYPE, 0));
    f
}

/// Element 0 is the null one, exactly as `emit` leaves it.
const ELEM_STRINGIFY: i32 = 1;
const ELEM_PARSE: i32 = 2;
/// The uniform signature is declared first in the module text, so it is
/// type 0.
const UNIFORM_TYPE: u32 = 0;

impl Engine {
    /// The engine as a module with no `throw` in it: a refusal is a trap that
    /// records its reason.
    fn new() -> Self {
        Self::build(None, Limits::default())
    }

    /// The same engine with a step budget a hundred-kilobyte document fits
    /// inside. See `a_hundred_kilobyte_document_needs_a_raised_step_budget`.
    fn generous() -> Self {
        Self::build(
            None,
            Limits {
                max_steps: 4_000_000_000,
                ..Limits::default()
            },
        )
    }

    /// The engine as `emit` will build it for a program that has `try`: a
    /// refusal is a throw in flight, and the `catch` entry below is what
    /// receives it.
    fn catching() -> Self {
        Self::build(Some(UNWIND), Limits::default())
    }

    fn build(unwind: Option<convert::Throwing>, limits: Limits) -> Self {
        let b = Bases::new(unwind);
        let funcs = b.funcs();
        let n = funcs.len() as u32;
        let adapter_stringify = n;
        let adapter_parse = n + 1;

        let (up, ur) = uniform();
        let mut out = String::from("(module\n");
        out.push_str(&format!(
            "  (type (func{}{}))\n",
            params_wat(&up),
            results_wat(&ur)
        ));
        out.push_str(&format!("  (memory {PAGES} 200)\n"));
        out.push_str(&format!(
            "  (global (mut i32) (i32.const {}))\n",
            b.pool.heap_start()
        ));
        if b.unwind.is_some() {
            out.push_str("  (global (mut i32) (i32.const 0))\n");
            out.push_str("  (global (mut i32) (i32.const 0))\n");
            out.push_str("  (global (mut i64) (i64.const 0))\n");
        }
        out.push_str(&format!("  (table {} funcref)\n", 3));
        let (offset, bytes) = b.pool.segment();
        out.push_str(&format!("  (data (i32.const {offset}) \""));
        for byte in bytes {
            out.push_str(&format!("\\{byte:02x}"));
        }
        out.push_str("\")\n");
        for f in &funcs {
            out.push_str(&func_wat(&f.params, &f.results, &f.locals, &f.body));
        }

        // The two adapters: forward what the target declares and let the rest
        // of the uniform parameter list fall away. Byte-for-byte the shape
        // `emit::m1::lower` writes for a user function that became a value.
        let stringify_at = b.js.func_base + Js::Stringify.offset();
        let parse_at = b.js.func_base + Js::Parse.offset();
        for (target, arity) in [(stringify_at, 3u32), (parse_at, 2u32)] {
            let body: Vec<Ins> = (0..arity * repr::WIDTH)
                .map(Ins::LocalGet)
                .chain(std::iter::once(Ins::Call(target)))
                .collect();
            out.push_str(&func_wat(&up, &ur, &[], &body));
        }
        out.push_str(&format!(
            "  (elem (i32.const 1) {adapter_stringify} {adapter_parse})\n"
        ));

        // The exports, in the order they are indexed below.
        let value = repr::SLOTS.to_vec();
        // Four entries: the one-argument spellings the corpus uses, and the
        // full-argument ones, so that a replacer, a space and a reviver can be
        // handed in and refused rather than talked about.
        for (key, argc) in [
            (b.js.names.stringify, 1u32),
            (b.js.names.parse, 1),
            (b.js.names.stringify, 3),
            (b.js.names.parse, 2),
        ] {
            let built = entry(&b.js, key, argc);
            let params: Vec<ValType> = (0..argc).flat_map(|_| repr::SLOTS).collect();
            out.push_str(&func_wat(
                &params,
                &value,
                &built.local_groups(),
                &built.body,
            ));
        }
        // `fleet.js`'s `call()`, spelled in wasm:
        //
        // ```js
        // try { return JSON.parse(resultJson); } catch (_err) { return resultJson; }
        // ```
        //
        // The flag read after the `call_indirect` is the check `emit` compiles
        // at any call site; clearing it and taking the handler's path is
        // `bind_caught`. Nothing here is special to JSON.
        if let Some(unwind) = b.unwind {
            let mut built = entry(&b.js, b.js.names.parse, 1);
            built.body.push(Ins::GlobalGet(unwind.flag));
            built.body.push(Ins::If(BlockType::Empty));
            built.body.push(Ins::I32Const(0));
            built.body.push(Ins::GlobalSet(unwind.flag));
            built.body.push(Ins::LocalGet(0));
            built.body.push(Ins::LocalGet(1));
            built.body.push(Ins::Return);
            built.body.push(Ins::End);
            out.push_str(&func_wat(
                &value,
                &value,
                &built.local_groups(),
                &built.body,
            ));
        }
        out.push_str(&format!(
            "  (func\n    i32.const {}\n    global.set 0\n  )\n",
            b.pool.heap_start()
        ));
        if let Some(unwind) = b.unwind {
            out.push_str(&format!(
                "  (func (result i32)\n    global.get {}\n  )\n",
                unwind.flag
            ));
            out.push_str(&format!(
                "  (func\n    i32.const 0\n    global.set {}\n  )\n",
                unwind.flag
            ));
        }
        out.push_str(&format!("  (export \"stringify\" (func {}))\n", n + 2));
        out.push_str(&format!("  (export \"parse\" (func {}))\n", n + 3));
        out.push_str(&format!("  (export \"stringify3\" (func {}))\n", n + 4));
        out.push_str(&format!("  (export \"parse2\" (func {}))\n", n + 5));
        if b.unwind.is_some() {
            out.push_str(&format!("  (export \"call\" (func {}))\n", n + 6));
            out.push_str(&format!("  (export \"reset\" (func {}))\n", n + 7));
            out.push_str(&format!("  (export \"flag\" (func {}))\n", n + 8));
            out.push_str(&format!("  (export \"unflag\" (func {}))\n", n + 9));
        } else {
            out.push_str(&format!("  (export \"reset\" (func {}))\n", n + 6));
        }
        out.push_str(")\n");

        let bytes = wat::parse_str(&out).expect("the printed text is valid wasm text");
        let module = WasmModule::from_bytes_with(&bytes, limits)
            .unwrap_or_else(|e| panic!("load gate rejected the module: {}", e.message()));
        let instance = module
            .instantiate()
            .unwrap_or_else(|e| panic!("instantiate failed: {}", e.message()));
        Engine {
            instance,
            bytes: bytes.len(),
        }
    }

    fn reset(&mut self) {
        self.instance
            .invoke_by_name("reset", &[])
            .expect("reset never traps");
    }

    /// Clear a throw the previous call left in flight, so one row of a table
    /// cannot pass because of the row before it.
    fn clear_flag(&mut self) {
        if self.instance.invoke_by_name("flag", &[]).is_ok() {
            self.instance
                .invoke_by_name("unflag", &[])
                .expect("clearing the flag never traps");
        }
    }

    /// Write a string record at [`SCRATCH`] and answer its address.
    fn text(&mut self, s: &str) -> i32 {
        let at = SCRATCH as usize;
        let mut view = self.instance.memory_mut().expect("guest memory");
        let mem: &mut [u8] = &mut view;
        mem[at..at + 4].copy_from_slice(&(s.len() as u32).to_le_bytes());
        mem[at + 4..at + 4 + s.len()].copy_from_slice(s.as_bytes());
        SCRATCH
    }

    fn read(&self, ptr: i32) -> String {
        let view = self.instance.memory().expect("guest memory");
        let bytes: &[u8] = &view;
        let at = ptr as usize;
        let len =
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("valid UTF-8")
    }

    fn fault(&self) -> i32 {
        let view = self.instance.memory().expect("guest memory");
        let bytes: &[u8] = &view;
        i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// The unwind flag, for the modes that have one. Read through an export
    /// rather than off the globals, which a module does not export.
    fn global_flag(&mut self) -> i32 {
        let out = self
            .instance
            .invoke_by_name("flag", &[])
            .expect("reading the flag never traps");
        let Val::I32(v) = out[0] else {
            panic!("expected an i32")
        };
        v
    }

    fn clear_fault(&mut self) {
        let mut view = self.instance.memory_mut().expect("guest memory");
        let mem: &mut [u8] = &mut view;
        mem[0..4].copy_from_slice(&0i32.to_le_bytes());
    }

    /// `JSON.stringify(v)`, as a JS value pair.
    fn stringify(&mut self, v: (i32, i64)) -> Result<(i32, i64), String> {
        self.call("stringify", v)
    }

    /// `JSON.parse(text)`, as a JS value pair.
    fn parse_value(&mut self, text: &str) -> Result<(i32, i64), String> {
        self.reset();
        self.clear_flag();
        let ptr = self.text(text);
        self.clear_fault();
        self.raw("parse", (TAG_STRING, i64::from(ptr)))
    }

    fn call(&mut self, name: &str, v: (i32, i64)) -> Result<(i32, i64), String> {
        self.reset();
        self.clear_fault();
        self.clear_flag();
        self.raw(name, v)
    }

    /// Invoke one of the wide entries with a full argument list.
    fn wide(&mut self, name: &str, args: &[(i32, i64)]) -> Result<(i32, i64), String> {
        self.reset();
        self.clear_fault();
        self.clear_flag();
        let mut vals = Vec::new();
        for (tag, payload) in args {
            vals.push(Val::I32(*tag));
            vals.push(Val::I64(*payload));
        }
        match self.instance.invoke_by_name(name, &vals) {
            Ok(out) => {
                let (Val::I32(tag), Val::I64(payload)) = (out[0], out[1]) else {
                    panic!("expected a JS value pair")
                };
                Ok((tag, payload))
            }
            Err(e) => Err(e.message().to_string()),
        }
    }

    fn raw(&mut self, name: &str, v: (i32, i64)) -> Result<(i32, i64), String> {
        match self
            .instance
            .invoke_by_name(name, &[Val::I32(v.0), Val::I64(v.1)])
        {
            Ok(out) => {
                let (Val::I32(tag), Val::I64(payload)) = (out[0], out[1]) else {
                    panic!("expected a JS value pair")
                };
                Ok((tag, payload))
            }
            Err(e) => Err(e.message().to_string()),
        }
    }

    /// `JSON.stringify(JSON.parse(text))`, the round trip the differential
    /// corpus runs.
    fn round_trip(&mut self, text: &str) -> Result<String, String> {
        self.reset();
        self.clear_flag();
        let ptr = self.text(text);
        self.clear_fault();
        let parsed = self.raw("parse", (TAG_STRING, i64::from(ptr)))?;
        let out = self.raw("stringify", parsed)?;
        assert_eq!(out.0, TAG_STRING, "stringify answered a non-String");
        Ok(self.read(out.1 as i32))
    }
}

fn params_wat(p: &[ValType]) -> String {
    if p.is_empty() {
        return String::new();
    }
    let mut s = String::from(" (param");
    for t in p {
        s.push(' ');
        s.push_str(ty_wat(*t));
    }
    s.push(')');
    s
}

fn results_wat(r: &[ValType]) -> String {
    if r.is_empty() {
        return String::new();
    }
    let mut s = String::from(" (result");
    for t in r {
        s.push(' ');
        s.push_str(ty_wat(*t));
    }
    s.push(')');
    s
}

// =========================================================================
// What the answers are checked against
// =========================================================================

/// Both sides reduced to one canonical text: keys sorted, every number an
/// `f64`. That is what makes `serde_json` usable as an oracle at all -- see
/// the file header for the three places it is not.
fn canon(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => {
            let x = n.as_f64().expect("a JSON number is a double here");
            format!("{x:?}")
        }
        serde_json::Value::String(s) => format!("{s:?}"),
        serde_json::Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canon).collect();
            format!("[{}]", inner.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| format!("{k:?}:{}", canon(&map[k.as_str()])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}
fn func_wat(
    params: &[ValType],
    results: &[ValType],
    locals: &[(u32, ValType)],
    body: &[Ins],
) -> String {
    let mut out = String::from("  (func");
    out.push_str(&params_wat(params));
    out.push_str(&results_wat(results));
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
        Ins::F64Trunc => "f64.trunc".into(),
        Ins::I32TruncF64S => "i32.trunc_f64_s".into(),
        Ins::I32WrapI64 => "i32.wrap_i64".into(),
        Ins::I64ExtendI32U => "i64.extend_i32_u".into(),
        Ins::F64ConvertI32S => "f64.convert_i32_s".into(),
        Ins::F64ReinterpretI64 => "f64.reinterpret_i64".into(),
        Ins::I64ReinterpretF64 => "i64.reinterpret_f64".into(),
    }
}

// =========================================================================
// A second harness: the script-level one
// =========================================================================
//
// The JSON harness above assembles a module by hand, because `emit.rs` does
// not yet know the JSON set exists. Unwinding *is* wired, so everything below
// goes through the ordinary door -- `compile_qjs_m1` -> the load gate ->
// `instantiate` -> `invoke_by_name("main")` -- and nothing about the emitted
// shape is read.

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
    Object,
    Function,
}

#[track_caller]
fn instantiate(source: &str) -> WasmInstance {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()))
}

#[track_caller]
fn run(source: &str) -> Out {
    let mut instance = instantiate(source);
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    decode(&instance, &vals, source)
}

#[track_caller]
fn decode(instance: &WasmInstance, vals: &[Val], source: &str) -> Out {
    match vals {
        [Val::I32(TAG_OBJECT), _] => return Out::Object,
        [Val::I32(t), _] if *t == repr::TAG_FUNCTION => return Out::Function,
        _ => {}
    }
    let value = Value::returned(vals)
        .unwrap_or_else(|e| panic!("{source:?}: cannot read the result back: {e}"));
    match value {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            let at = ptr as usize;
            let len =
                u32::from_le_bytes([view[at], view[at + 1], view[at + 2], view[at + 3]]) as usize;
            Out::Str(String::from_utf8(view[at + 4..at + 4 + len].to_vec()).expect("utf-8"))
        }
    }
}

fn script_fault(instance: &WasmInstance) -> i32 {
    let view = instance.memory().expect("guest memory");
    i32::from_le_bytes([view[0], view[1], view[2], view[3]])
}

/// How many globals a compiled script declares. The unwind channel is three
/// of them, so this is the observable answer to "does this module carry
/// unwinding machinery at all".
fn globals_of(source: &str) -> u32 {
    let bytes = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let mut at = 8;
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let (size, next) = uleb(&bytes, at);
        at = next;
        if id == 6 {
            return uleb(&bytes, at).0 as u32;
        }
        at += size;
    }
    0
}

fn uleb(bytes: &[u8], mut at: usize) -> (usize, usize) {
    let (mut value, mut shift) = (0usize, 0);
    loop {
        let byte = bytes[at];
        at += 1;
        value |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return (value, at);
        }
        shift += 7;
    }
}

// =========================================================================
// DEFECT 1 -- the in-flight flag outlives the call that raised it
// =========================================================================

/// A module global is instance state, and an uncaught `throw` used to leave
/// the in-flight flag **set** when it trapped. `emit.rs`'s entry-point
/// prologue clears the fault word (`runtime::clear_fault`) precisely so that
/// the word describes *this* call; the flag beside it was not cleared, so it
/// described the *previous* one.
///
/// tinyvm instances are persistent by design and a top-level call is the unit
/// of budget -- `crates/tinyvm/src/wasm.rs:1418`: "Maximum instructions
/// executed by one top-level call ... the next top-level call receives a
/// fresh budget" -- so a second `invoke_by_name` on the same instance is the
/// supported shape, not an exotic one.
///
/// The failure was **silent**: the second call contains no `throw` on any path
/// it takes, and its `catch` ran anyway, bound to the value the first call
/// threw.
///
/// FIXED: two instructions in the entry-point prologue beside
/// `runtime::clear_fault` -- `i32.const 0; global.set <flag>` -- paid only by
/// the modules that already carry the channel. A poisoned instance now answers
/// exactly what a fresh one does, which is the assertion below.
#[test]
fn a_handled_throw_is_never_a_throw_the_previous_call_raised() {
    // $0 chooses whether call 1 throws. Call 2 passes 0, so no `throw`
    // statement is on its path at all.
    let source = "function f() { return 42; } \
                  if ($0 === 1) { throw \"boom\"; } \
                  try { return f(); } catch (e) { return \"caught \" + e; }";
    let mut instance = instantiate(source);

    let first = instance.invoke_by_name("main", &Value::args(&[Value::Number(1.0)]));
    assert!(first.is_err(), "call 1 throws and nothing catches it");
    assert_eq!(script_fault(&instance), runtime::FAULT_UNCAUGHT_THROW);

    let second = instance
        .invoke_by_name("main", &Value::args(&[Value::Number(0.0)]))
        .expect("call 2 returns");
    assert_eq!(
        decode(&instance, &second, source),
        Out::Number(42.0),
        "call 2 has no reachable `throw`, so its `catch` must not run"
    );

    // The same answer from an instance that was never poisoned, which is what
    // makes the row above a comparison and not a guess.
    let mut clean = instantiate(source);
    let only = clean
        .invoke_by_name("main", &Value::args(&[Value::Number(0.0)]))
        .expect("no trap");
    assert_eq!(decode(&clean, &only, source), Out::Number(42.0));

    // And the channel still works *after* the poisoning window: a call that
    // does throw is still caught by its own handler.
    let mut both = instantiate(source);
    let _ = both.invoke_by_name("main", &Value::args(&[Value::Number(1.0)]));
    let third = both
        .invoke_by_name("main", &Value::args(&[Value::Number(1.0)]))
        .err();
    assert!(third.is_some(), "an uncaught throw is still uncaught");
}

/// The same defect where nothing catches: the second call used to *trap*, and
/// the fault word told the host an uncaught throw happened in a call that has
/// no reachable `throw`. It returns now, and the fault word is clear.
#[test]
fn a_call_after_an_uncaught_throw_starts_from_a_clear_channel() {
    let source = "function f() { return 42; } if ($0 === 1) { throw 1; } return f();";
    let mut instance = instantiate(source);
    let _ = instance.invoke_by_name("main", &Value::args(&[Value::Number(1.0)]));
    let second = instance
        .invoke_by_name("main", &Value::args(&[Value::Number(0.0)]))
        .expect("the second call has no reachable `throw`");
    assert_eq!(decode(&instance, &second, source), Out::Number(42.0));
    assert_eq!(script_fault(&instance), runtime::FAULT_NONE);

    let mut clean = instantiate(source);
    assert!(
        clean
            .invoke_by_name("main", &Value::args(&[Value::Number(0.0)]))
            .is_ok()
    );
}

/// The worst shape the leak had: the stale value was a *reference*, so the
/// second call's `catch` received a pointer into the heap the first call
/// allocated in. Now the second call never enters its handler at all.
#[test]
fn the_previous_calls_object_is_never_handed_to_this_calls_catch() {
    let source = "const f = function () { return 42; }; \
                  if ($0 === 1) { throw { secret: 9 }; } \
                  try { return f(); } catch (e) { return e.secret; }";
    let mut instance = instantiate(source);
    let _ = instance.invoke_by_name("main", &Value::args(&[Value::Number(1.0)]));
    let second = instance
        .invoke_by_name("main", &Value::args(&[Value::Number(0.0)]))
        .expect("returns");
    assert_eq!(decode(&instance, &second, source), Out::Number(42.0));
}

// =========================================================================
// A `finally` block's completion value, which used to escape
// =========================================================================

/// ECMA-262 14.15.3, `try Block Finally`:
///
/// ```text
/// 1. Let B be Completion(Evaluation of Block).
/// 2. Let F be Completion(Evaluation of Finally).
/// 3. If F is a normal completion, set F to B.
/// 4. Return ? UpdateEmpty(F, undefined).
/// ```
///
/// Step 3 was the one that was missing: a finalizer that completes
/// **normally** contributes nothing to the value, so `try { 1; } finally { 2; }`
/// is `1`. The engine lowered the finalizer as an ordinary statement list and
/// the last expression statement in it overwrote the completion slot.
///
/// It is not an exotic shape: `finally { cleanup = true; }` is an assignment,
/// and an assignment is an expression statement with a value. Every row below
/// was run against node 24 to get the second column.
///
/// FIXED: `Lower::try_finally` holds the pending completion in a scratch pair
/// across the finalizer and puts it back. Only the normal path reads it -- the
/// two abrupt paths carry their value in `Finalizer::slot` -- so the restore
/// needs no guard.
#[test]
fn a_normally_completing_finally_keeps_the_pending_completion_value() {
    // (source, ECMA-262 and node, what the finalizer's own value would be)
    let rows: &[(&str, f64, f64)] = &[
        ("try { 1; } finally { 2; }", 1.0, 2.0),
        ("let s = 0; try { 1; } finally { s = 2; }", 1.0, 2.0),
        ("try { throw 1; } catch (e) { 5; } finally { 9; }", 5.0, 9.0),
        ("try { 1; } finally { if (true) { 7; } }", 1.0, 7.0),
        (
            "let i = 0; try { 1; } finally { while (i < 2) { i = i + 1; } }",
            1.0,
            2.0,
        ),
    ];
    for (source, spec, finalizers_own) in rows {
        assert_ne!(spec, finalizers_own, "a row that proves nothing");
        assert_eq!(run(source), Out::Number(*spec), "14.15.3 step 3: {source}");
    }

    // The two shapes that were already right, kept so the fix is located
    // rather than described: an *empty* finalizer leaves the value alone, and
    // so does a `catch` -- 14.15.3's `try Block Catch` has no step 3 to miss.
    assert_eq!(run("try { 1; } finally { }"), Out::Number(1.0));
    assert_eq!(run("try { 1; } catch (e) { }"), Out::Number(1.0));

    // And the three things step 3 must NOT do. The finalizer still runs; an
    // abrupt finalizer still replaces what was pending (14.15.3's last step);
    // and a pending `return` still carries its own value past it.
    assert_eq!(
        run("let n = 0; try { 1; } finally { n = 5; } return n;"),
        Out::Number(5.0)
    );
    assert_eq!(
        run("function f() { try { return 1; } finally { return 2; } } return f();"),
        Out::Number(2.0)
    );
    assert_eq!(
        run("function f() { try { return 1; } finally { 9; } } return f();"),
        Out::Number(1.0)
    );
}

// =========================================================================
// The fault code, at the door a host actually reads
// =========================================================================

/// `runtime::FAULT_UNCAUGHT_THROW` exists so a host can tell "your script
/// threw" from "your script is broken" -- that argument is written at
/// `src/runtime.rs`'s constant. The public door is `tinyvm_qjs::guest_fault`,
/// and it used to match only `FAULT_HEAP_EXHAUSTED`, so the new code fell into
/// `_ => None` -- the same answer an ordinary guest fault gives. The
/// capability was emitted and not delivered, and the suite missed it because
/// `conditional_and_try.rs` reads the raw word out of linear memory rather
/// than through the function a host would call.
///
/// FIXED: one variant, `GuestFault::UncaughtThrow`, and one arm. This test is
/// deliberately written through the public door and not through the word.
#[test]
fn an_uncaught_throw_is_visible_through_the_public_guest_fault_door() {
    let mut throwing = instantiate("throw 1;");
    let error = throwing
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err("an uncaught throw traps");
    assert_eq!(error.message(), "unreachable executed");
    let memory = throwing.memory().expect("guest memory");
    // The word is written...
    assert_eq!(script_fault(&throwing), runtime::FAULT_UNCAUGHT_THROW);
    // ...and the reader sees it.
    assert_eq!(
        tinyvm_qjs::guest_fault(&memory),
        Some(tinyvm_qjs::GuestFault::UncaughtThrow),
    );

    // A genuinely broken script, for the comparison the host has to make. It
    // is a different answer now, which is the whole point of the code.
    let mut broken = instantiate("const u = undefined; return u.a;");
    let _ = broken.invoke_by_name("main", &Value::args(&[]));
    assert_eq!(
        tinyvm_qjs::guest_fault(&broken.memory().expect("guest memory")),
        None,
        "a broken script is not a throw"
    );

    // And a throw that is caught leaves nothing at the door at all.
    let mut caught = instantiate("try { throw 1; } catch (e) { return e; }");
    assert!(caught.invoke_by_name("main", &Value::args(&[])).is_ok());
    assert_eq!(
        tinyvm_qjs::guest_fault(&caught.memory().expect("guest memory")),
        None
    );
}

// =========================================================================
// The unwind channel is declared for the thing that needs it most
// =========================================================================

/// `Scan::throws` used to be set in exactly one place -- the `StmtKind::Throw`
/// arm -- so a program whose only abrupt completion came from a *runtime*
/// refusal got no channel.
///
/// That was correct while nothing but `throw` raised. It stopped being correct
/// the moment `JSON` was wired: `src/convert.rs` states the condition in its
/// own words -- "**A program that mentions `JSON` needs the channel whether or
/// not it writes `throw`**, because `JSON.parse` can raise one. That is a
/// condition on `emit`'s scan" -- and `emit.rs` did not satisfy it. With
/// `unwind: None`, `convert::build_json`'s `__throw` records
/// `FAULT_UNCAUGHT_THROW` and traps instead of returning, so the `catch`
/// would never have run.
///
/// The shape that lands on is `fleet.js` lines 15-19, the reason the feature
/// was built:
///
/// ```js
/// try { return JSON.parse(resultJson); } catch (_err) { return resultJson; }
/// ```
///
/// -- a `try`/`catch` with no `throw` statement anywhere in the file.
///
/// FIXED: `scan` ends with `out.throws |= out.json`. This test pins both
/// halves -- the channel is there when `JSON` is named, and still absent when
/// nothing can raise.
#[test]
fn a_program_that_names_json_declares_the_channel_json_raises_through() {
    // One global is the bump pointer; two per script binding. `x` and the
    // catch parameter are two bindings, so five is "no channel" -- and a
    // `try`/`catch` alone still declares none, because nothing on any path
    // through it can raise.
    assert_eq!(
        globals_of("let x = 0; try { x = 1; } catch (e) { x = 2; } return x;"),
        5,
        "a `try`/`catch` with nothing that can raise declares no channel"
    );
    // One `throw` anywhere and the three appear.
    assert_eq!(
        globals_of("let x = 0; try { throw 1; } catch (e) { x = 2; } return x;"),
        8
    );
    // And so does one mention of `JSON`, with no `throw` in the text at all:
    // three for the channel and two more for the namespace object.
    assert_eq!(
        globals_of("let x = 0; try { x = JSON.parse(\"1\"); } catch (e) { x = 2; } return x;"),
        5 + 3 + 2
    );
    // The observable half, which is the one that matters: the refusal is
    // caught rather than trapping.
    let mut instance = instantiate(
        "function call(s) { try { return JSON.parse(s); } catch (_err) { return s; } } \
         return call(\"not json\");",
    );
    let out = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("the `catch` runs; without the channel this traps");
    assert_eq!(
        decode(&instance, &out, "fleet's call()"),
        Out::Str("not json".into())
    );
}

// =========================================================================
// The label question: fifty shapes that came back clean
// =========================================================================

/// Where a throw goes is a label index the compiler computes as
/// `self.depth - target`, and a single miscounted `end` would aim every
/// non-local branch at the wrong label while leaving the module well-typed.
/// So the branch is checked by observing where control actually arrived, over
/// every shape that puts a call, a handler or a finalizer at an unusual
/// depth.
///
/// All of these are correct. The list is the evidence for that, and it is
/// also the list a future `break`/`continue` -- the other non-local target --
/// has to keep passing.
#[test]
fn a_throw_reaches_the_handler_the_specification_names() {
    let rows: &[(&str, Out)] = &[
        // -- crossing frames -------------------------------------------------
        (
            "function h() { throw \"h\"; } function g() { h(); return \"g\"; } \
             function f() { try { g(); } catch (e) { return \"caught:\" + e; } return \"no\"; } return f();",
            Out::Str("caught:h".into()),
        ),
        (
            "function r(n) { if (n === 0) { throw \"deep\"; } return r(n - 1); } \
             try { r(50); } catch (e) { return e; } return \"no\";",
            Out::Str("deep".into()),
        ),
        (
            "function a(n) { if (n === 0) { throw \"bottom\"; } return b(n - 1); } \
             function b(n) { return a(n); } try { a(20); } catch (e) { return e; } return \"no\";",
            Out::Str("bottom".into()),
        ),
        // -- through a value, and through an adapter that drops arguments -----
        (
            "const g = function () { throw 7; }; try { g(); } catch (e) { return e; } return 0;",
            Out::Number(7.0),
        ),
        (
            "const f = function (a, b, c) { throw a; }; try { f(1); } catch (e) { return e; } return \"no\";",
            Out::Number(1.0),
        ),
        (
            "const f = function (a) { throw a; }; try { f(5, 6, 7); } catch (e) { return e; } return \"no\";",
            Out::Number(5.0),
        ),
        (
            "const a = function () { throw \"A\"; }; const b = function () { return a(); }; \
             const c = function () { return b(); }; try { c(); } catch (e) { return e; } return \"no\";",
            Out::Str("A".into()),
        ),
        // -- out of every expression position --------------------------------
        (
            "function g() { throw \"a\"; } function h(x, y) { return x + y; } \
             try { return h(1, g()); } catch (e) { return e; }",
            Out::Str("a".into()),
        ),
        (
            "function g() { throw \"c\"; } try { return true ? g() : 0; } catch (e) { return e; }",
            Out::Str("c".into()),
        ),
        (
            "function g() { throw \"t\"; } try { return g() ? 1 : 2; } catch (e) { return e; }",
            Out::Str("t".into()),
        ),
        (
            "function g() { throw \"?\"; } try { return false ? 1 : (true ? g() : 2); } catch (e) { return e; }",
            Out::Str("?".into()),
        ),
        (
            "function g() { throw \"&\"; } try { return true && g(); } catch (e) { return e; }",
            Out::Str("&".into()),
        ),
        (
            "function g() { throw \"|\"; } try { return false || g(); } catch (e) { return e; }",
            Out::Str("|".into()),
        ),
        (
            "function g() { throw \"p\"; } const o = { a: 0 }; \
             try { o.a = g(); } catch (e) { return e + \":\" + o.a; } return \"no\";",
            Out::Str("p:0".into()),
        ),
        (
            "function g() { throw \"q\"; } const o = { a: 1 }; \
             try { o.a += g(); } catch (e) { return e + \":\" + o.a; } return \"no\";",
            Out::Str("q:1".into()),
        ),
        (
            "function g() { throw \"k\"; } const o = {}; try { o[g()] = 1; } catch (e) { return e; } return \"no\";",
            Out::Str("k".into()),
        ),
        (
            "function g() { throw \"o\"; } try { const o = { a: 1, b: g(), c: 3 }; } catch (e) { return e; } return \"no\";",
            Out::Str("o".into()),
        ),
        // -- out of every loop header ----------------------------------------
        (
            "function g() { throw \"w\"; } try { while (g()) { } } catch (e) { return e; } return 0;",
            Out::Str("w".into()),
        ),
        (
            "function g() { throw \"i\"; } try { for (let i = g(); i < 3; i = i + 1) { } } catch (e) { return e; } return \"no\";",
            Out::Str("i".into()),
        ),
        (
            "function g() { throw \"j\"; } try { for (let i = 0; g(); i = i + 1) { } } catch (e) { return e; } return \"no\";",
            Out::Str("j".into()),
        ),
        (
            "function g() { throw \"u\"; } try { for (let i = 0; i < 3; i = g()) { } } catch (e) { return e; } return \"no\";",
            Out::Str("u".into()),
        ),
        (
            "let n = 0; function f() { let i = 0; while (i < 10) { if (i === 3) { throw i; } i = i + 1; n = n + 1; } return -1; } \
             try { f(); } catch (e) { return e * 100 + n; } return -2;",
            Out::Number(303.0),
        ),
        // -- handlers at unequal depths --------------------------------------
        (
            "try { try { throw 1; } catch (e) { throw 2; } } catch (e) { return e; }",
            Out::Number(2.0),
        ),
        (
            "try { throw 1; } catch (a) { try { throw 2; } catch (b) { return a + b; } } return 0;",
            Out::Number(3.0),
        ),
        (
            "let s = \"\"; try { try { throw \"a\"; } catch (e) { try { throw \"b\"; } finally { s = s + \"F\"; } } } \
             catch (e) { return s + e; } return \"no\";",
            Out::Str("Fb".into()),
        ),
        (
            "let s = \"\"; try { try { throw \"a\"; } finally { try { throw \"b\"; } catch (e) { s = s + e; } } } \
             catch (e) { return s + e; } return \"no\";",
            Out::Str("ba".into()),
        ),
        (
            "try { throw 1; } catch (e) { function g() { throw \"g\"; } try { g(); } catch (h) { return h; } } return \"no\";",
            Out::Str("g".into()),
        ),
        // -- `finally` on each of its three paths ----------------------------
        (
            "let n = 0; function f() { try { throw 1; } finally { n = n + 10; } } \
             try { f(); } catch (e) { n = n + e; } return n;",
            Out::Number(11.0),
        ),
        (
            "try { try { throw 1; } finally { throw 2; } } catch (e) { return e; }",
            Out::Number(2.0),
        ),
        (
            "function f() { try { return 1; } finally { return 2; } } return f();",
            Out::Number(2.0),
        ),
        (
            "function f() { try { throw \"x\"; } finally { return \"fin\"; } } return f();",
            Out::Str("fin".into()),
        ),
        (
            "function f() { try { try { throw 1; } catch (e) { throw 2; } } finally { return 3; } } return f();",
            Out::Number(3.0),
        ),
        (
            "let s = \"\"; try { try { try { throw \"x\"; } finally { s = s + \"1\"; } } finally { s = s + \"2\"; } } \
             catch (e) { s = s + e; } return s;",
            Out::Str("12x".into()),
        ),
        (
            "let s = \"\"; function f() { try { try { return \"r\"; } finally { s = s + \"1\"; } } finally { s = s + \"2\"; } } \
             const v = f(); return s + v;",
            Out::Str("12r".into()),
        ),
        (
            "let s = \"\"; function f() { try { try { throw 1; } catch (e) { return \"c\"; } } finally { s = s + \"F\"; } } \
             const v = f(); return s + v;",
            Out::Str("Fc".into()),
        ),
        // A finalizer that calls a function which throws and catches
        // *internally* must not lose the throw it is standing on: the parked
        // value is a local and the callee overwrites the globals.
        (
            "function q() { try { throw \"inner\"; } catch (e) { return 0; } } \
             try { try { throw \"outer\"; } finally { q(); } } catch (e) { return e; } return \"lost\";",
            Out::Str("outer".into()),
        ),
        (
            "const g = function () { throw \"gf\"; }; \
             try { try { throw \"a\"; } finally { g(); } } catch (e) { return e; } return \"lost\";",
            Out::Str("gf".into()),
        ),
        (
            "let s = \"\"; try { try { throw \"z\"; } finally { let i = 0; while (i < 3) { s = s + \"f\"; i = i + 1; } } } \
             catch (e) { return s + e; } return \"no\";",
            Out::Str("fffz".into()),
        ),
        // Sequential statements reusing the same scratch locals.
        (
            "let s = \"\"; try { s = s + \"1\"; } finally { s = s + \"a\"; } \
             try { throw \"x\"; } catch (e) { s = s + e; } finally { s = s + \"b\"; } \
             try { s = s + \"3\"; } finally { s = s + \"c\"; } return s;",
            Out::Str("1axb3c".into()),
        ),
        (
            "let n = 0; let i = 0; while (i < 3) { try { try { throw 1; } finally { n = n + 1; } } catch (e) { n = n + 10; } i = i + 1; } return n;",
            Out::Number(33.0),
        ),
        // -- the thrown value is any value -----------------------------------
        (
            "const fv = function () { return 1; }; try { throw fv; } catch (e) { return typeof e; }",
            Out::Str("function".into()),
        ),
        (
            "try { throw { a: 1 }; } catch (e) { return e.a; }",
            Out::Number(1.0),
        ),
        (
            "try { throw undefined; } catch (e) { return typeof e; }",
            Out::Str("undefined".into()),
        ),
        (
            "try { throw null; } catch (e) { return typeof e; }",
            Out::Str("object".into()),
        ),
        (
            "try { throw { a: 1 }; } catch (e) { return e; }",
            Out::Object,
        ),
        (
            "const fv = function () { return 1; }; try { throw fv; } catch (e) { return e; }",
            Out::Function,
        ),
        (
            "function g() { throw \"x\" + \"y\" + 1; } try { g(); } catch (e) { return e; } return \"no\";",
            Out::Str("xy1".into()),
        ),
        // -- the catch parameter is an ordinary binding -----------------------
        (
            "try { throw 1; } catch (e) { e = e + 1; return e; }",
            Out::Number(2.0),
        ),
        (
            "let e = \"outer\"; try { throw \"inner\"; } catch (e) { } return e;",
            Out::Str("outer".into()),
        ),
        (
            "try { throw 1; } catch { return \"bare\"; }",
            Out::Str("bare".into()),
        ),
    ];
    for (source, want) in rows {
        assert_eq!(run(source), *want, "{source}");
    }
    println!("{} unwinding shapes checked", rows.len());
}

/// A refusal that cannot be raised is still a refusal that has to arrive
/// honestly. An exhausted heap while a throw is parked is reported as an
/// exhausted heap, not as the throw that happened to be in flight.
#[test]
fn a_heap_exhausted_during_unwinding_is_still_reported_as_an_exhausted_heap() {
    // Two pages is 128 KiB; doubling a string forty times asks for far more.
    let tight = Limits {
        max_memory_pages: 2,
        ..Limits::default()
    };
    const BIG: &str = "function big() { let s = \"x\"; let i = 0; while (i < 40) { s = s + s; i = i + 1; } return s; } ";
    for (what, tail) in [
        (
            "in a finalizer with a throw parked",
            "try { throw \"pending\"; } finally { big(); }",
        ),
        (
            "in the try block",
            "try { big(); } catch (e) { return \"caught\"; }",
        ),
        (
            "in the catch block",
            "try { throw 1; } catch (e) { big(); }",
        ),
        (
            "building the thrown value",
            "try { throw big(); } catch (e) { return \"caught\"; }",
        ),
    ] {
        let source = format!("{BIG}{tail}");
        let wasm = compile_qjs_m1(&source).expect("compiles");
        let module = WasmModule::from_bytes_with(&wasm, tight).expect("clears the gate");
        let mut instance = module.instantiate().expect("instantiates");
        let error = instance.invoke_by_name("main", &Value::args(&[]));
        assert!(error.is_err(), "{what}: expected a trap");
        assert_eq!(
            tinyvm_qjs::guest_fault(&instance.memory().expect("guest memory")),
            Some(tinyvm_qjs::GuestFault::HeapExhausted),
            "{what}: the budget failure was reported as something else"
        );
    }
}

/// Syntax nested past the compiler's own frame budget is a diagnostic, not a
/// stack overflow -- `try`/`catch` did not open a new way to abort the
/// process.
#[test]
fn try_nested_past_the_frame_budget_is_a_diagnostic_and_not_an_abort() {
    let deep = format!(
        "{}throw 1;{}",
        "try { ".repeat(2000),
        " } catch (e) { }".repeat(2000)
    );
    let message = compile_qjs_m1(&deep).expect_err("refused").message;
    assert!(
        message.contains("nested deeper than this engine's"),
        "got {message}"
    );
    // And a depth inside the budget still compiles and runs.
    let ok = format!(
        "{}throw 1;{}",
        "try { ".repeat(50),
        " } catch (e) { }".repeat(50)
    );
    assert_eq!(
        run(&format!("{ok} return \"survived\";")),
        Out::Str("survived".into())
    );
}

// =========================================================================
// JSON, attacked
// =========================================================================
//
// `src/emit.rs` does not mention `Js`, `JsonCtx` or `JSON_SET`, so none of
// this is reachable from a script yet -- see
// `defect_json_is_not_reachable_from_a_script` below. The harness above is
// the same stand-in `tests/json.rs` uses: the namespace object, an
// `__obj_get` for the property, and a `call_indirect` through an adapter.

/// `JSON` resolves to **this engine's own object** and not to a host import.
///
/// Under `Names::HostImport` a bare `JSON` used to compile to a zero-parameter,
/// two-result import `js.JSON` -- one V1 pair, opaque to the compiler -- and
/// `JSON.parse` was then an ordinary property read on whatever the host
/// answered. `tinyvm_qjs::Value` has no Object variant, so no host could answer
/// with an object that has a `parse` property, and reading a property of a
/// primitive traps. The nine `JSON` sites in `fleet.js` compiled and the one
/// path every other method routes through could not run.
///
/// Under the default `Names` the same source was refused outright.
///
/// FIXED: `ast::Res::Json`. The scope walk still runs first, so this is one
/// bound name and not a global scope -- `control_conformance.rs`'s
/// `json_is_an_ordinary_name_and_a_script_may_take_it` is where that half is
/// held. What is held here is the attacker's question: can a host still get
/// between a script and its `JSON`?
#[test]
fn no_host_can_get_between_a_script_and_its_json() {
    use tinyvm_qjs::{Names, Options, compile_qjs_m1_with};

    // The default naming answers, where it used to refuse.
    assert_eq!(
        run("return JSON.parse(\"1\");"),
        Out::Number(1.0),
        "the default naming reaches `JSON`"
    );

    // The naming `fleet.js` is compiled with: a host import for every free
    // name -- except this one.
    for source in [
        "return JSON.parse(\"1\");",
        "return JSON.stringify(1);",
        "try { return JSON.parse(\"[\"); } catch (e) { return \"caught\"; }",
    ] {
        let wasm = compile_qjs_m1_with(
            source,
            Options {
                names: Names::HostImport,
            },
        )
        .expect("compiles");
        let module =
            WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the gate");
        let imports: Vec<(String, usize, usize)> = module
            .imports()
            .iter()
            .map(|i| (format!("{}.{}", i.module, i.field), i.n_params, i.n_results))
            .collect();
        assert!(
            imports.is_empty(),
            "{source} still reaches JSON through the host door: {imports:?}"
        );
    }
}

/// An array anywhere in the text parses, and this row is the *product*
/// consequence of that, because it is not written down anywhere else.
///
/// This test used to be `an_array_anywhere_in_the_text_is_refused_by_name`.
/// `fleet.js` exists to parse whatever the Fleet broker answered, and an
/// answer that is or contains a JSON array -- `tabs.list` is the obvious one
/// -- took the `catch` and came back as raw text, so a caller expecting a
/// value got a string. The Array milestone is what changed that, and these
/// are the same five documents answering correctly.
#[test]
fn an_array_anywhere_in_the_text_parses() {
    let mut e = Engine::new();
    for text in [
        "[]",
        "[1,2]",
        r#"{"tabs":[]}"#,
        r#"{"a":{"b":[1]}}"#,
        "[[[]]]",
    ] {
        assert!(
            serde_json::from_str::<serde_json::Value>(text).is_ok(),
            "{text} is valid JSON, so this row means what it says"
        );
        e.clear_fault();
        let outcome = e.parse_value(text);
        assert!(outcome.is_ok(), "an array was refused: {text}");
        assert_eq!(e.fault(), 0, "{text} recorded a throw it did not raise");
    }
}

/// A hundred kilobytes of JSON, parsed and printed back.
///
/// Not a benchmark: the question is whether the bump heap, the object record
/// and the output buffer stay correct at a size no unit test reaches.
///
/// The first thing it found is a **budget**, not a bug, and it is worth
/// writing down because a downstream embedder will meet it: a 100 KB document
/// does not fit inside `Limits::default().max_steps`
/// (`crates/tinyvm/src/wasm.rs:1418` -- "Maximum instructions executed by one
/// top-level call"). The refusal is honest -- a `"step budget"` trap, the
/// guest's fault word untouched -- and the number below is what a host has to
/// raise it to. With the budget raised, the round trip is exact.
#[test]
fn a_hundred_kilobyte_document_round_trips_once_the_step_budget_allows_it() {
    // ~100 KB of nested objects with distinct keys and every scalar shape.
    let mut text = String::from("{");
    let mut n = 0usize;
    while text.len() < 100_000 {
        if n > 0 {
            text.push(',');
        }
        text.push_str(&format!(
            r#""k{n}":{{"i":{n},"f":{n}.5,"s":"v{n}","t":true,"z":null}}"#
        ));
        n += 1;
    }
    text.push('}');
    assert!(
        text.len() >= 100_000,
        "the document is {} bytes",
        text.len()
    );

    // Under the default budget it stops, and stops honestly.
    let mut stingy = Engine::new();
    let refusal = stingy
        .round_trip(&text)
        .expect_err("100 KB does not fit the default step budget");
    assert_eq!(refusal, "step budget");
    assert_ne!(
        stingy.fault(),
        runtime::FAULT_UNCAUGHT_THROW,
        "a host budget was reported as a JavaScript throw"
    );

    // The largest power-of-two prefix of the same shape that *does* fit, so
    // the number is a measurement and not a guess.
    let mut fits = 0usize;
    for members in [1usize, 8, 64, 128, 256, 512, 1024] {
        let mut probe = String::from("{");
        for i in 0..members {
            if i > 0 {
                probe.push(',');
            }
            probe.push_str(&format!(
                r#""k{i}":{{"i":{i},"f":{i}.5,"s":"v{i}","t":true,"z":null}}"#
            ));
        }
        probe.push('}');
        match stingy.round_trip(&probe) {
            Ok(out) => {
                assert_eq!(out, probe);
                fits = probe.len();
            }
            Err(_) => break,
        }
    }
    println!("largest round trip under the default step budget: {fits} bytes");

    // With the budget raised, the whole document.
    let mut e = Engine::generous();
    let oracle: serde_json::Value =
        serde_json::from_str(&text).expect("the oracle reads its own document");
    let got = e
        .round_trip(&text)
        .unwrap_or_else(|err| panic!("trap on a {}-byte document: {err}", text.len()));
    let ours: serde_json::Value = serde_json::from_str(&got).expect("our output is JSON");
    assert_eq!(canon(&ours), canon(&oracle));
    assert_eq!(
        got, text,
        "insertion order and formatting are preserved verbatim"
    );
    println!(
        "{} bytes in, {} bytes out, {n} members",
        text.len(),
        got.len()
    );

    // One 100 KB *string*, which exercises the byte buffer rather than the
    // object record.
    let long = "a".repeat(100_000);
    let text = serde_json::to_string(&long).expect("prints");
    assert_eq!(e.round_trip(&text).expect("no trap"), text);
}

/// Every escape `JSONString` has, on the way **in**, including the ones no
/// round trip preserves verbatim.
///
/// The escapes `\b \f \n \r \t \" \\ \/` and `\uXXXX` all decode, a `\u0000`
/// survives inside a string, a surrogate *pair* becomes one code point, and a
/// **lone** surrogate is refused rather than becoming a malformed UTF-8
/// string in the guest heap -- which is the row that matters, because the
/// guest's string record is UTF-8 and a lone surrogate has no UTF-8 encoding.
#[test]
fn every_json_escape_decodes_and_a_lone_surrogate_is_refused() {
    let mut e = Engine::new();
    // (input text, what the round trip prints)
    let rows: &[(&str, &str)] = &[
        (r#""\"""#, r#""\"""#),
        (r#""\\""#, r#""\\""#),
        (r#""\/""#, r#""/""#),
        (r#""\b""#, r#""\b""#),
        (r#""\f""#, r#""\f""#),
        (r#""\n""#, r#""\n""#),
        (r#""\r""#, r#""\r""#),
        (r#""\t""#, r#""\t""#),
        (r#""\u0000""#, r#""\u0000""#),
        (r#""\u001f""#, r#""\u001f""#),
        (r#""\u0020""#, r#"" ""#),
        (r#""\u007f""#, "\"\u{7f}\""),
        (r#""\u00e9""#, "\"\u{e9}\""),
        (r#""\u07ff""#, "\"\u{7ff}\""),
        (r#""\u0800""#, "\"\u{800}\""),
        (r#""\uffff""#, "\"\u{ffff}\""),
        (r#""\ud83d\ude00""#, "\"\u{1f600}\""),
        (r#""\udbff\udfff""#, "\"\u{10ffff}\""),
        (r#""\u0041\u0042""#, r#""AB""#),
        (r#""a\u2028b\u2029c""#, "\"a\u{2028}b\u{2029}c\""),
        (r#""\uD83D\uDE00""#, "\"\u{1f600}\""),
    ];
    for (text, want) in rows {
        assert_eq!(
            e.round_trip(text)
                .unwrap_or_else(|err| panic!("trap on {text}: {err}")),
            *want,
            "round trip of {text}"
        );
    }

    // Lone surrogates, high and low, alone and mispaired.
    for text in [
        r#""\ud800""#,
        r#""\udc00""#,
        r#""\ud800x""#,
        r#""\ud800\u0041""#,
        r#""\udc00\ud800""#,
        r#""\ud800\ud800""#,
    ] {
        let outcome = e.parse_value(text);
        assert!(outcome.is_err(), "a lone surrogate was accepted: {text}");
        assert_eq!(
            e.fault(),
            runtime::FAULT_UNCAUGHT_THROW,
            "{text} trapped without recording a throw"
        );
    }

    // The escapes the grammar does *not* have.
    for text in [
        r#""\x41""#,
        r#""\'""#,
        r#""\0""#,
        r#""\u12""#,
        r#""\u00g0""#,
        r#""\ ""#,
    ] {
        assert!(serde_json::from_str::<serde_json::Value>(text).is_err());
        assert!(e.parse_value(text).is_err(), "accepted {text}");
    }
}

/// Malformed input **at every position**: every proper prefix of a valid
/// document, and every single-byte substitution in it.
///
/// The property is one sentence: this engine accepts a text exactly when
/// `serde_json` does. A prefix that happens to be valid on its own (`{"a":1`
/// truncated to `1`... there are a few) must be accepted; every other one
/// must be refused, and refused as a *throw* -- so the guest's own fault word
/// says so -- rather than as a bare trap, a hang or a wrong value.
///
/// Arrays are excluded from the corpus: this engine refuses them by design,
/// which the test above covers.
#[test]
fn malformed_input_at_every_position_is_refused_and_never_guessed() {
    let mut e = Engine::new();
    let seeds: Vec<String> = vec![
        r#"{"a":1}"#.to_string(),
        r#"{"a":"b","c":null}"#.to_string(),
        r#"{"n":-1.25e-10,"t":true,"f":false,"z":null}"#.to_string(),
        r#"{"s":"\u00e9\ud83d\ude00\n\t"}"#.to_string(),
        r#"{"a":{"b":{"c":{"d":0}}}}"#.to_string(),
        " \t\r\n{\t\"a\"\n:\r1 }\t".to_string(),
        "123456789012345678901234567890".to_string(),
        r#""caf\u00e9""#.to_string(),
    ];
    let mut prefixes = 0usize;
    let mut mutations = 0usize;
    let mut disagreements = Vec::new();

    for seed in &seeds {
        let bytes = seed.as_bytes();

        // Every proper prefix.
        for cut in 0..bytes.len() {
            let Ok(text) = std::str::from_utf8(&bytes[..cut]) else {
                continue;
            };
            prefixes += 1;
            let oracle = oracle_accepts(text);
            let ours = e.parse_value(text).is_ok();
            if ours != oracle {
                disagreements.push(format!("prefix {text:?}: oracle={oracle} ours={ours}"));
            }
            if !ours {
                assert_eq!(
                    e.fault(),
                    runtime::FAULT_UNCAUGHT_THROW,
                    "prefix {text:?} refused without recording a throw"
                );
            }
        }

        // Every single-byte substitution, over a spread of replacements that
        // covers the structural characters, a digit, a letter, whitespace, a
        // quote, a backslash and a high byte.
        for at in 0..bytes.len() {
            for byte in [
                b'{', b'}', b'[', b']', b'"', b'\\', b':', b',', b'0', b'9', b'-', b'+', b'.',
                b'e', b'x', b' ', b'\n', 0x00, 0x7f,
            ] {
                let mut copy = bytes.to_vec();
                copy[at] = byte;
                let Ok(text) = std::str::from_utf8(&copy) else {
                    continue;
                };
                // An array anywhere is a refusal this engine owns, so those
                // mutants are not a disagreement to count.
                if text.contains('[') || text.contains(']') {
                    continue;
                }
                mutations += 1;
                let oracle = oracle_accepts(text);
                let ours = e.parse_value(text).is_ok();
                if ours != oracle {
                    disagreements.push(format!("mutant {text:?}: oracle={oracle} ours={ours}"));
                }
            }
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} of {} texts disagreed with the oracle:\n{}",
        disagreements.len(),
        prefixes + mutations,
        disagreements
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!(
        "{prefixes} prefixes and {mutants_note} mutants agreed with serde_json",
        mutants_note = mutations
    );
}

/// `serde_json` refuses a number whose magnitude is outside `f64`; ECMA-262
/// 7.1.4.1 rounds it to Infinity and `JSON.parse` answers with it. That is
/// the oracle caveat `tests/json.rs`'s header records, and this is the one
/// place the fuzz above has to know about it: those texts *are* valid JSON,
/// so the oracle's answer is corrected rather than the engine's excused.
fn oracle_accepts(text: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(_) => true,
        Err(err) => err.to_string().contains("number out of range"),
    }
}

/// Bytes that are not UTF-8 at all, inside a string and outside one.
///
/// The guest reads a `[len][bytes]` record and never sees a `&str`. A script
/// cannot build such a record -- a JavaScript String literal is decoded by
/// the lexer -- but a **host** can: a `HostResult::Bytes` answer is copied
/// into a guest string record with its length checked and its contents not.
///
/// What is asserted is what happens, because it is a decision and not an
/// accident: bytes go through unexamined, so a non-UTF-8 byte inside a JSON
/// string comes back inside the parsed String -- a *key* string included, so
/// `{"a\xc3":1}` is an object with a key the host cannot print -- and a
/// non-UTF-8 byte in *structural* position is refused by the grammar like any
/// other stray byte.
/// Nothing reads out of bounds and nothing loops.
///
/// DIVERGENCE (host boundary, not this milestone's): the resulting String is
/// one `tinyvm_qjs::Value::String` cannot be read back as text. The same is
/// true of any host `Bytes` answer, so the place to settle it is the host
/// door, not the JSON parser.
#[test]
fn a_non_utf8_byte_goes_through_the_string_and_is_refused_outside_one() {
    let mut e = Engine::new();
    // (raw input bytes, accepted?, the bytes the parsed String holds)
    let rows: &[(&[u8], Option<&[u8]>)] = &[
        (b"\"\xff\"", Some(b"\xff")),
        (b"\"a\x80b\"", Some(b"a\x80b")),
        (b"\"\xed\xa0\x80\"", Some(b"\xed\xa0\x80")),
        (b"\xff", None),
        (b"{\"a\":\xff}", None),
        (b"\xffnull", None),
    ];
    for (raw, want) in rows {
        e.reset();
        e.clear_flag();
        e.clear_fault();
        let at = SCRATCH as usize;
        {
            let mut view = e.instance.memory_mut().expect("guest memory");
            let mem: &mut [u8] = &mut view;
            mem[at..at + 4].copy_from_slice(&(raw.len() as u32).to_le_bytes());
            mem[at + 4..at + 4 + raw.len()].copy_from_slice(raw);
        }
        match (e.raw("parse", (TAG_STRING, i64::from(SCRATCH))), want) {
            (Err(_), None) => {
                assert_eq!(
                    e.fault(),
                    runtime::FAULT_UNCAUGHT_THROW,
                    "{raw:?} was refused without recording a throw"
                );
            }
            (Ok((tag, payload)), Some(bytes)) => {
                assert_eq!(tag, TAG_STRING, "{raw:?} did not parse to a String");
                let view = e.instance.memory().expect("guest memory");
                let mem: &[u8] = &view;
                let ptr = payload as usize;
                let len = u32::from_le_bytes([mem[ptr], mem[ptr + 1], mem[ptr + 2], mem[ptr + 3]])
                    as usize;
                assert_eq!(&mem[ptr + 4..ptr + 4 + len], *bytes, "{raw:?}");
                assert!(
                    std::str::from_utf8(&mem[ptr + 4..ptr + 4 + len]).is_err(),
                    "this row is only interesting if the bytes are not UTF-8"
                );
            }
            (Ok(v), None) => panic!("{raw:?} was accepted as {v:?}"),
            (Err(m), Some(_)) => panic!("{raw:?} was refused: {m}"),
        }
    }
}

/// Deep nesting on the way **out**, not only on the way in: a structure the
/// parser accepted has to be printable, and where it is not, the refusal has
/// to arrive as a fault and not as a wrong string.
#[test]
fn deep_nesting_survives_the_round_trip_or_stops_honestly() {
    let mut e = Engine::new();
    let mut deepest = 0usize;
    for depth in [1usize, 16, 64, 128, 256, 512, 1024] {
        let text = format!("{}0{}", r#"{"n":"#.repeat(depth), "}".repeat(depth));
        match e.round_trip(&text) {
            Ok(out) => {
                assert_eq!(out, text, "round trip at depth {depth}");
                deepest = depth;
            }
            Err(message) => {
                assert_ne!(
                    e.fault(),
                    runtime::FAULT_UNCAUGHT_THROW,
                    "a host recursion limit at depth {depth} was reported as a JavaScript throw: {message}"
                );
                break;
            }
        }
    }
    assert!(deepest >= 64, "only {deepest} levels round-tripped");
    println!("deepest nesting that round-tripped: {deepest}");
}

/// With an unwind channel a refusal is a value a `catch` can hold, and it is
/// the **same** refusal the trapping mode records -- so a program does not
/// get a different answer for having a `try` in it somewhere else.
#[test]
fn the_two_unwind_modes_refuse_the_same_texts() {
    let mut trapping = Engine::new();
    let mut catching = Engine::catching();
    let texts = [
        "",
        "{",
        "}",
        "nul",
        "01",
        "1.",
        ".1",
        "+1",
        "'a'",
        r#"{"a":}"#,
        r#"{a:1}"#,
        r#"{"a":1,}"#,
        "[1]",
        r#""\ud800""#,
        "tru",
        "NaN",
        "Infinity",
        "--1",
        "1e",
        "{}{}",
        r#""unterminated"#,
    ];
    for text in texts {
        let trapped = trapping.parse_value(text).is_err();
        // In catching mode the call returns and the flag is what says so.
        catching.reset();
        catching.clear_flag();
        catching.clear_fault();
        let ptr = catching.text(text);
        let answered = catching.raw("parse", (TAG_STRING, i64::from(ptr)));
        let flagged = answered.is_err() || catching.global_flag() == 1;
        assert_eq!(
            trapped, flagged,
            "{text:?}: trapping mode says {trapped}, catching mode says {flagged}"
        );
    }
}
