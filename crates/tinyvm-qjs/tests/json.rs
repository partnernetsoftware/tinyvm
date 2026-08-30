//! `JSON.parse` and `JSON.stringify` (ECMA-262 25.5), executed for real.
//!
//! # Why this file assembles the modules itself
//!
//! `JSON` is **a namespace object holding two function values**, not a
//! compiler intrinsic, and this file proves that claim by building exactly
//! what `emit.rs` would build and nothing more: `__json_ns` makes the object,
//! `__obj_get` reads `stringify` or `parse` off it, and the call goes through
//! `call_indirect` on a two-element table of adapters. `src/emit.rs` is
//! another lane's file, so the wiring is a hook the integrator makes and this
//! harness stands in for it -- the same arrangement `tests/conversions.rs`
//! uses, and for the same reason.
//!
//! Every entry point below therefore runs the *script-shaped* path. There is
//! no direct call to `__json_stringify` anywhere in this file, because a
//! direct call would prove something weaker than what the design claims.
//!
//! # The oracle, and the three places it is wrong
//!
//! `serde_json` is the differential oracle. It is an excellent oracle for two
//! of the three things this milestone has to get right -- **which texts are
//! valid JSON**, and **how a string is escaped** -- and it is the wrong oracle
//! for the third:
//!
//! - **Number formatting.** `serde_json` prints an integer-valued `1` as `1`
//!   only because it kept it as an integer; `1.0` comes back as `1.0`, and
//!   ECMA-262 6.1.6.1.20 says `1`. Numbers are therefore compared *as numbers*
//!   (both sides parsed back), and the formatting itself is
//!   `tests/conversions.rs`'s subject, where it is checked against the spec
//!   text rather than against a second formatter.
//! - **Object key order.** `serde_json::Value` is a `BTreeMap` by default, so
//!   it sorts keys and cannot witness insertion order at all. Order is
//!   therefore asserted textually here, against ECMA-262 10.1.11.1.
//! - **Numbers out of `f64` range.** `serde_json` *rejects* `1e400`; ECMA-262
//!   makes it `Infinity`, which `JSON.stringify` then writes as `null`. That
//!   row is checked against the spec.
//!
//! There was a fourth, and it is a dependency line rather than a caveat:
//! `serde_json`'s default float parser is not correctly rounded, so it reads
//! back a double it printed itself as a *different* double. The generated
//! corpus found it on `1.001592857142857e-295`, where this engine agreed with
//! serde's text and disagreed with serde's value. The dev-dependency therefore
//! turns on `float_roundtrip`; an oracle that does not round-trip cannot judge
//! one that does.

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
    BlockType, Ins, TAG_BOOL, TAG_NULL, TAG_NUMBER, TAG_OBJECT, TAG_STRING, TAG_UNDEFINED, ValType,
    box_object, const_undefined, unbox_function,
};
use runtime::{
    ALIGN_WORD, Conversions, Ctx as RtCtx, FN_ELEMENT, FnBuild, PrimNames, RtFunc, StringPool,
};
use tinyvm::{Limits, Val, WasmModule};

// =========================================================================
// Harness
// =========================================================================

/// Where the test writes a string record it wants the guest to read. Well
/// above anything one call allocates, and the `reset` export puts the bump
/// pointer back before every call, so the guest never reaches it.
const SCRATCH: i32 = 200_000;
/// Eight pages, so [`SCRATCH`] is addressable without the guest having to
/// grow memory first.
const PAGES: u32 = 8;

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
            string_member: false,
            unwind: None,
            type_error: None,
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
            // The array set follows the JSON set in the module, which is where
            // `emit` puts it and what `JsonCtx::beside` computes.
            arrays: json_base + convert::JSON_SET.len() as u32,
            captures: false,
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
        // The array set follows the JSON set and is not optional here:
        // `JSON.parse` calls `__arr_new` and `__arr_push`, and `__json_ser`
        // dispatches to `__json_ser_arr`. A fixture that left it out would
        // build a module whose JSON functions call whatever happens to sit at
        // those indices -- which is exactly what the load gate answered with
        // "validation: type mismatch" when this was first assembled.
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
        Self::build(None)
    }

    /// The engine as `emit` will build it for a program that has `try`: a
    /// refusal is a throw in flight, and the `catch` entry below is what
    /// receives it.
    fn catching() -> Self {
        Self::build(Some(UNWIND))
    }

    fn build(unwind: Option<convert::Throwing>) -> Self {
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
        let module = WasmModule::from_bytes_with(&bytes, Limits::default())
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

// =========================================================================
// The tests
// =========================================================================

#[test]
fn the_module_clears_the_load_gate_and_json_is_an_object_with_two_function_properties() {
    let mut e = Engine::new();
    // Reaching `stringify` at all is the proof: `entry` reads the property off
    // the namespace object and calls it through `call_indirect`. A `JSON` that
    // were a compiler intrinsic would not need a table.
    let out = e.stringify((TAG_NULL, 0)).expect("no trap");
    assert_eq!(out.0, TAG_STRING);
    assert_eq!(e.read(out.1 as i32), "null");
    println!("module is {} bytes", e.bytes);
}

#[test]
fn stringify_answers_the_seven_primitive_shapes() {
    let mut e = Engine::new();
    let cases: Vec<((i32, i64), Option<&str>)> = vec![
        ((TAG_NULL, 0), Some("null")),
        ((TAG_BOOL, 1), Some("true")),
        ((TAG_BOOL, 0), Some("false")),
        ((TAG_NUMBER, 1.5f64.to_bits() as i64), Some("1.5")),
        ((TAG_NUMBER, 0f64.to_bits() as i64), Some("0")),
        ((TAG_NUMBER, (-0f64).to_bits() as i64), Some("0")),
        ((TAG_NUMBER, f64::NAN.to_bits() as i64), Some("null")),
        ((TAG_NUMBER, f64::INFINITY.to_bits() as i64), Some("null")),
        (
            (TAG_NUMBER, f64::NEG_INFINITY.to_bits() as i64),
            Some("null"),
        ),
        ((TAG_UNDEFINED, 0), None),
    ];
    for (input, want) in cases {
        let out = e.stringify(input).expect("no trap");
        match want {
            Some(text) => {
                assert_eq!(out.0, TAG_STRING, "{input:?}");
                assert_eq!(e.read(out.1 as i32), text, "{input:?}");
            }
            None => assert_eq!(out.0, TAG_UNDEFINED, "{input:?}"),
        }
    }
}

#[test]
fn stringify_escapes_exactly_what_25_5_2_2_escapes() {
    let mut e = Engine::new();
    for probe in [
        "",
        "plain",
        "quote\"inside",
        "back\\slash",
        "\u{8}\u{9}\u{a}\u{c}\u{d}",
        "\u{0}\u{1}\u{1f}",
        "\u{b}\u{e}",
        "slash/kept",
        "caf\u{e9} \u{4e2d}\u{6587} \u{1f600}",
    ] {
        e.reset();
        let ptr = e.text(probe);
        let out = e.stringify((TAG_STRING, i64::from(ptr))).expect("no trap");
        let got = e.read(out.1 as i32);
        let want = serde_json::to_string(probe).expect("serde can print a string");
        assert_eq!(got, want, "escaping {probe:?}");
    }
}

#[test]
fn stringify_writes_objects_in_insertion_order_and_omits_undefined() {
    let mut e = Engine::new();
    // The order is the parse order, which is the insertion order, which is
    // ECMA-262 10.1.11.1's -- and it is not alphabetical, which is what makes
    // this assertion worth making.
    assert_eq!(
        e.round_trip(r#"{"b":1,"a":2}"#).expect("no trap"),
        r#"{"b":1,"a":2}"#
    );
    assert_eq!(e.round_trip("{}").expect("no trap"), "{}");
    assert_eq!(
        e.round_trip(r#"{"z":{"y":{"x":null}}}"#).expect("no trap"),
        r#"{"z":{"y":{"x":null}}}"#
    );
}

#[test]
fn parse_accepts_the_json_grammar_and_the_round_trip_agrees_with_serde() {
    let mut e = Engine::new();
    let corpus = valid_corpus();
    for text in &corpus {
        let oracle: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|err| panic!("the oracle rejected a valid text {text:?}: {err}"));
        let got = e
            .round_trip(text)
            .unwrap_or_else(|err| panic!("trap on {text:?}: {err}"));
        let ours: serde_json::Value = serde_json::from_str(&got)
            .unwrap_or_else(|err| panic!("our own output {got:?} is not JSON: {err}"));
        assert_eq!(canon(&ours), canon(&oracle), "round trip of {text:?}");
    }
    println!("{} valid texts round-tripped", corpus.len());
}

#[test]
fn parse_rejects_every_form_the_json_grammar_excludes() {
    let mut e = Engine::new();
    let corpus = invalid_corpus();
    for (text, why) in &corpus {
        assert!(
            serde_json::from_str::<serde_json::Value>(text).is_err(),
            "the oracle accepted {text:?}, so this row is not an invalid form"
        );
        let outcome = e.parse_value(text);
        assert!(outcome.is_err(), "we accepted {text:?} ({why})");
        assert_eq!(
            e.fault(),
            runtime::FAULT_UNCAUGHT_THROW,
            "{text:?} ({why}) trapped without recording a throw"
        );
    }
    println!("{} invalid texts refused", corpus.len());
}

#[test]
fn where_the_oracle_and_the_spec_disagree_the_spec_is_what_is_implemented() {
    let mut e = Engine::new();

    // `-0`. ECMA-262 6.1.6.1.20 step 2 answers `"0"` for both zeros, so
    // `JSON.stringify(-0)` is `"0"` -- and every other JavaScript engine
    // agrees. `serde_json` prints `-0.0`, because it is not that algorithm.
    // The *value* is still negative zero, which is what the second half of
    // this checks: the round trip loses the sign only at the printer, exactly
    // where the spec loses it.
    assert_eq!(serde_json::to_string(&-0.0f64).expect("prints"), "-0.0");
    assert_eq!(e.round_trip("-0").expect("no trap"), "0");
    let parsed = e.parse_value("-0").expect("no trap");
    assert_eq!(parsed.0, TAG_NUMBER);
    assert_eq!(
        f64::from_bits(parsed.1 as u64).to_bits(),
        (-0.0f64).to_bits(),
        "the parsed value is negative zero even though it prints as 0"
    );

    // A number past the `f64` range. `serde_json` refuses the text; ECMA-262
    // 7.1.4.1 rounds to Infinity, and 25.5.2.2 step 10 then writes `null`.
    assert!(serde_json::from_str::<serde_json::Value>("1e400").is_err());
    let parsed = e.parse_value("1e400").expect("no trap");
    assert_eq!(parsed.0, TAG_NUMBER);
    assert_eq!(f64::from_bits(parsed.1 as u64), f64::INFINITY);
    assert_eq!(e.round_trip("1e400").expect("no trap"), "null");
    assert_eq!(e.round_trip("-1e400").expect("no trap"), "null");
    assert_eq!(e.round_trip("1e-400").expect("no trap"), "0");

    // U+2028 and U+2029 are ordinary characters to 25.5.2.2, and this engine
    // leaves them alone -- which is also what `serde_json` does, so the row is
    // here as a decision recorded rather than a divergence.
    e.reset();
    let ptr = e.text("a\u{2028}b");
    let out = e.stringify((TAG_STRING, i64::from(ptr))).expect("no trap");
    assert_eq!(e.read(out.1 as i32), "\"a\u{2028}b\"");
}

#[test]
fn a_cycle_is_refused_rather_than_run_forever() {
    // There is no way to build a cycle from JSON text, so this one is built
    // by hand: an object whose only property points at itself.
    let mut e = Engine::new();
    let outcome = e.parse_value(r#"{"self":null}"#).expect("no trap");
    assert_eq!(outcome.0, TAG_OBJECT);
    let obj = outcome.1 as i32;
    // Overwrite the single entry's value with the object itself.
    let entries = {
        let view = e.instance.memory().expect("guest memory");
        let bytes: &[u8] = &view;
        i32::from_le_bytes([
            bytes[obj as usize + 8],
            bytes[obj as usize + 9],
            bytes[obj as usize + 10],
            bytes[obj as usize + 11],
        ]) as usize
    };
    {
        let mut view = e.instance.memory_mut().expect("guest memory");
        let mem: &mut [u8] = &mut view;
        mem[entries + 4..entries + 8].copy_from_slice(&TAG_OBJECT.to_le_bytes());
        mem[entries + 8..entries + 16].copy_from_slice(&i64::from(obj).to_le_bytes());
    }
    e.clear_fault();
    let outcome = e.raw("stringify", (TAG_OBJECT, i64::from(obj)));
    assert!(outcome.is_err(), "a cycle was serialized");
    assert_eq!(e.fault(), runtime::FAULT_UNCAUGHT_THROW);
}

#[test]
fn an_undefined_or_function_property_is_left_out_comma_and_all() {
    // 25.5.2.4 step 5. Neither value can be reached from JSON text, so both
    // are written straight into an object the parser just built -- which is
    // also the only way to be sure the *comma* is decided by the same test
    // that decides the key.
    let mut e = Engine::new();
    for (victim, want) in [(0usize, r#"{"b":2}"#), (1, r#"{"a":1}"#)] {
        for tag in [TAG_UNDEFINED, repr::TAG_FUNCTION] {
            let parsed = e.parse_value(r#"{"a":1,"b":2}"#).expect("no trap");
            let obj = parsed.1 as i32;
            let entries = {
                let view = e.instance.memory().expect("guest memory");
                let bytes: &[u8] = &view;
                i32::from_le_bytes([
                    bytes[obj as usize + 8],
                    bytes[obj as usize + 9],
                    bytes[obj as usize + 10],
                    bytes[obj as usize + 11],
                ]) as usize
            };
            {
                let at = entries + victim * 16 + 4;
                let mut view = e.instance.memory_mut().expect("guest memory");
                let mem: &mut [u8] = &mut view;
                mem[at..at + 4].copy_from_slice(&tag.to_le_bytes());
            }
            let out = e
                .raw("stringify", (TAG_OBJECT, i64::from(obj)))
                .expect("no trap");
            assert_eq!(e.read(out.1 as i32), want, "tag {tag} at property {victim}");
        }
    }
}

#[test]
fn a_program_that_never_writes_json_carries_none_of_this() {
    // The gate is the whole reason [`Js`] is a second set rather than more
    // rows in `Cv`. It is checked structurally, because the alternative --
    // trusting that `emit` remembers -- is the thing that would quietly stop
    // being true.
    let b = Bases::new(None);
    let base: Vec<&str> = runtime::build(&b.rt)
        .into_iter()
        .chain(convert::build(&b.cv))
        .map(|f| f.name)
        .collect();
    assert_eq!(base.len(), runtime::SET.len() + convert::SET.len());
    for js in convert::JSON_SET {
        assert!(
            !base.contains(&js.symbol()),
            "{} is in the unconditional sets",
            js.symbol()
        );
    }
}

#[test]
fn a_replacer_a_space_and_a_reviver_are_refused_rather_than_ignored() {
    let mut e = Engine::new();
    let undef = (TAG_UNDEFINED, 0i64);
    let null = (TAG_NULL, 0i64);
    let two = (TAG_NUMBER, 2f64.to_bits() as i64);

    // Absent is fine, and so is `null`, which is what a script writes when it
    // wants to skip the replacer and pass a space.
    assert!(e.wide("stringify3", &[null, undef, undef]).is_ok());
    assert!(e.wide("stringify3", &[null, null, null]).is_ok());
    e.reset();
    let one = (TAG_STRING, i64::from(e.text("1")));
    assert!(e.wide("parse2", &[one, undef]).is_ok());

    // A space that would change the answer must not be silently dropped.
    assert!(
        e.wide("stringify3", &[null, undef, two]).is_err(),
        "a space argument was accepted and ignored"
    );
    assert_eq!(e.fault(), runtime::FAULT_UNCAUGHT_THROW);
    assert!(
        e.wide("stringify3", &[null, two, undef]).is_err(),
        "a replacer was accepted and ignored"
    );
    assert_eq!(e.fault(), runtime::FAULT_UNCAUGHT_THROW);

    e.reset();
    let text = (TAG_STRING, i64::from(e.text("1")));
    assert!(
        e.wide("parse2", &[text, two]).is_err(),
        "a reviver was accepted and ignored"
    );
    assert_eq!(e.fault(), runtime::FAULT_UNCAUGHT_THROW);
}

#[test]
fn every_refusal_names_its_message_in_exactly_one_place() {
    // The thrown value cannot be read back -- `__throw` traps and there is no
    // `catch` to receive it -- so *which* message each refusal chose is
    // checked where it is decided: in the emitted instruction list. Without
    // this, "the array refusal names the engine's boundary" would be a claim
    // about a string nothing proves is reachable.
    let b = Bases::new(Some(UNWIND));
    let funcs = convert::build_json(&b.js);
    let names = b.js.names;
    // `names.array` left this table when the Array milestone landed the type:
    // `[1,2]` is parsed now, so there is no refusal to place. The cycle
    // message gained a second home for the same reason -- an array that
    // contains itself is the same TypeError an object that does gets.
    let expected: [(i32, &str, usize); 7] = [
        (names.surrogate, "__json_pstr", 4),
        (names.cycle, "__json_ser_obj", 1),
        (names.cycle, "__json_ser_arr", 1),
        (names.replacer, "__json_stringify", 2),
        (names.reviver, "__json_parse", 1),
        (names.eof, "__json_pstr", 1),
        (names.syntax, "__json_pval", 4),
    ];
    for (address, symbol, count) in expected {
        let f = funcs
            .iter()
            .find(|f| f.name == symbol)
            .expect("the function is in the set");
        let seen = f
            .body
            .iter()
            .filter(|ins| **ins == Ins::I32Const(address))
            .count();
        assert_eq!(seen, count, "{symbol} names the message at {address}");
    }
    // And the array parser refuses malformed *text* with the syntax message,
    // never with a sentence about this engine's boundary: `[1,` is a broken
    // document and `[1,2]` is a document this engine now reads. Getting this
    // backwards is what the deleted `names.array` row used to guard against
    // in the other direction.
    let parr = funcs
        .iter()
        .find(|f| f.name == "__json_parr")
        .expect("the array parser is in the set");
    assert_eq!(
        parr.body
            .iter()
            .filter(|ins| **ins == Ins::I32Const(names.syntax))
            .count(),
        1,
        "the array parser's one refusal is a syntax error"
    );
}

#[test]
fn a_generated_corpus_round_trips_against_serde() {
    let mut e = Engine::new();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut checked = 0usize;
    for _ in 0..400 {
        let value = rng.value(3);
        let text = serde_json::to_string(&value).expect("serde prints its own value");
        let got = e
            .round_trip(&text)
            .unwrap_or_else(|err| panic!("trap on generated {text:?}: {err}"));
        let ours: serde_json::Value = serde_json::from_str(&got)
            .unwrap_or_else(|err| panic!("our output {got:?} is not JSON: {err}"));
        assert_eq!(
            canon(&ours),
            canon(&value),
            "generated {text:?} came back as {got:?}"
        );
        checked += 1;
    }
    println!("{checked} generated documents round-tripped");
}

/// xorshift64*, so the corpus is the same corpus on every machine and every
/// run. A failure that cannot be reproduced is not a failure anyone can fix.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Every character class the escaper and the decoder have an arm for.
    fn text(&mut self) -> String {
        let alphabet: [char; 16] = [
            'a',
            'Z',
            '0',
            ' ',
            '"',
            '\\',
            '/',
            '\u{8}',
            '\u{9}',
            '\u{a}',
            '\u{c}',
            '\u{d}',
            '\u{1}',
            '\u{e9}',
            '\u{4e2d}',
            '\u{1f600}',
        ];
        let n = self.below(8);
        (0..n).map(|_| alphabet[self.below(16) as usize]).collect()
    }

    fn value(&mut self, depth: u32) -> serde_json::Value {
        let pick = if depth == 0 {
            self.below(4)
        } else {
            self.below(5)
        };
        match pick {
            0 => serde_json::Value::Null,
            1 => serde_json::Value::Bool(self.below(2) == 1),
            2 => {
                // Whole and fractional, tiny and huge -- the four shapes
                // `Number::toString` lays out differently.
                let scale = [1.0, 1e-7, 1e21, 1e-300][self.below(4) as usize];
                let x =
                    (self.below(1 << 20) as f64) * scale / [1.0, 3.0, 7.0][self.below(3) as usize];
                serde_json::Number::from_f64(x)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }
            3 => serde_json::Value::String(self.text()),
            _ => {
                let n = self.below(4);
                let mut map = serde_json::Map::new();
                for _ in 0..n {
                    map.insert(self.text(), self.value(depth - 1));
                }
                serde_json::Value::Object(map)
            }
        }
    }
}

#[test]
fn nesting_deeper_than_the_host_allows_is_a_refusal_and_not_a_hang() {
    let mut e = Engine::new();
    // The recursion is `__json_pval` -> `__json_pobj` -> `__json_pval`, on the
    // *host's* call stack, so where it stops is tinyvm's bound and not this
    // module's. What matters is that it stops the way a fault stops: a typed
    // error, with the guest's own fault word untouched, so a host can tell it
    // apart from a `throw`.
    let mut deepest = 0usize;
    for depth in [1usize, 16, 64, 256, 1024, 4096] {
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
                    "a host limit was reported as a JavaScript throw"
                );
                println!("depth {depth} refused: {message}");
                break;
            }
        }
    }
    println!("deepest nesting that round-tripped: {deepest}");
    assert!(deepest >= 64, "64 levels is the least this has to carry");
}

#[test]
fn with_an_unwind_channel_a_bad_parse_is_caught_and_fleet_js_falls_back() {
    // The whole point of the exercise: `fleet.js` reads whatever the host
    // answered, tries to parse it, and hands back the raw text when it is not
    // JSON. That behaviour is a `catch` over a `JSON.parse`, and here it is,
    // running.
    let mut e = Engine::catching();
    for (text, want_parsed) in [
        (r#"{"ok":true}"#, true),
        ("plain broker text, not JSON", false),
        ("broker_invalid_arguments: tabs.set-note", false),
        // `[1,2,3]` used to be in the `false` column, and it was the row that
        // mattered: a broker answer that is an array took this `catch` and
        // came back as text. The Array milestone moved it to `true`.
        ("[1,2,3]", true),
        ("", false),
        ("42", true),
    ] {
        e.reset();
        e.clear_flag();
        let ptr = e.text(text);
        e.clear_fault();
        let out = e
            .raw("call", (TAG_STRING, i64::from(ptr)))
            .unwrap_or_else(|err| panic!("the catch did not catch {text:?}: {err}"));
        if want_parsed {
            assert_ne!(
                (out.0, out.1),
                (TAG_STRING, i64::from(ptr)),
                "{text:?} should have parsed"
            );
        } else {
            assert_eq!(
                (out.0, out.1),
                (TAG_STRING, i64::from(ptr)),
                "{text:?} should have fallen back to the raw text"
            );
        }
        assert_eq!(
            e.fault(),
            0,
            "a caught throw must leave no fault behind for {text:?}"
        );
    }
}

#[test]
fn a_throw_in_flight_leaves_every_function_in_the_set() {
    // The propagation checks, exercised where each of them is: a throw raised
    // deep inside a nested document has to come back out through
    // `__json_pval`, `__json_pobj`, `__json_parr` and `__json_parse` without
    // any of them carrying on. If one check were missing the answer would be a
    // half-built object rather than the fallback.
    //
    // The first row used to be `{"a":{"b":{"c":[1]}}}`, which raised because
    // the innermost value was an array. It parses now, so the row that
    // exercises the same depth raises for a reason that is still a reason --
    // and two rows were added to carry the throw out through the *array*
    // parser, which the object parser's checks say nothing about.
    let mut e = Engine::catching();
    for text in [
        r#"{"a":{"b":{"c":[nope]}}}"#,
        r#"{"a":[1,[2,{"b":nope}]]}"#,
        r#"[1,2,"#,
        r#"{"a":1,"b":{"c":nope}}"#,
        r#"{"a":"\ud800"}"#,
        r#"{"a":"\uZZ00"}"#,
        r#"{"a":01}"#,
        r#"{"a":1,}"#,
        r#"{"a":1"#,
    ] {
        e.reset();
        e.clear_flag();
        let ptr = e.text(text);
        e.clear_fault();
        let out = e
            .raw("call", (TAG_STRING, i64::from(ptr)))
            .unwrap_or_else(|err| panic!("{text:?} was not caught: {err}"));
        assert_eq!(
            (out.0, out.1),
            (TAG_STRING, i64::from(ptr)),
            "{text:?} should have fallen back"
        );
    }
}

#[test]
fn a_thrown_stringify_is_caught_too() {
    // The other direction, and the arm that is easiest to get wrong: a cycle
    // is raised inside `__json_ser_obj`'s loop, so it has to leave the loop,
    // then `__json_ser`, then `__json_stringify`.
    let mut e = Engine::catching();
    let parsed = e
        .parse_value(r#"{"self":null,"after":1}"#)
        .expect("no trap");
    let obj = parsed.1 as i32;
    let entries = {
        let view = e.instance.memory().expect("guest memory");
        let bytes: &[u8] = &view;
        i32::from_le_bytes([
            bytes[obj as usize + 8],
            bytes[obj as usize + 9],
            bytes[obj as usize + 10],
            bytes[obj as usize + 11],
        ]) as usize
    };
    {
        let mut view = e.instance.memory_mut().expect("guest memory");
        let mem: &mut [u8] = &mut view;
        mem[entries + 4..entries + 8].copy_from_slice(&TAG_OBJECT.to_le_bytes());
        mem[entries + 8..entries + 16].copy_from_slice(&i64::from(obj).to_le_bytes());
    }
    e.clear_fault();
    let out = e
        .raw("stringify", (TAG_OBJECT, i64::from(obj)))
        .expect("the throw returned rather than trapping");
    // The value it answered with is garbage by construction -- what matters is
    // that the flag is up, which is what a `catch` reads.
    let _ = out;
    assert_eq!(e.global_flag(), 1, "the cycle left no throw in flight");
    assert_eq!(e.fault(), 0, "a catchable throw is not a fault");
}

#[test]
fn without_an_unwind_channel_the_same_refusals_are_recorded_traps() {
    // The other body of `__throw`. Same refusals, same one function deciding,
    // and a host that reads the fault word can still tell what happened.
    let mut e = Engine::new();
    // `[1]` used to lead this list, as the engine-boundary refusal. It parses
    // now, so the list is what it always should have been: text that is not
    // JSON, and text this engine's String cannot hold.
    for text in ["nope", "[1,", r#"{"a":"\ud800"}"#] {
        assert!(e.parse_value(text).is_err(), "{text:?}");
        assert_eq!(e.fault(), runtime::FAULT_UNCAUGHT_THROW, "{text:?}");
    }
}

#[test]
fn the_emitted_size_of_the_json_set_is_written_down() {
    let b = Bases::new(None);
    let with = encoded(&b, true);
    let without = encoded(&b, false);
    println!(
        "the JSON set costs {} bytes ({with} with, {without} without)",
        with - without
    );
    let catching = wat_size(&Bases::new(Some(UNWIND)));
    println!(
        "with the unwind channel it is {catching}, so parking a throw and the \
         seven propagation checks cost {} bytes more",
        catching - with
    );
}

/// The three sets encoded, whatever mode they were built in.
fn wat_size(b: &Bases) -> usize {
    let funcs = b.funcs();
    let mut out = String::from("(module\n  (memory 1 200)\n");
    out.push_str(&format!(
        "  (global (mut i32) (i32.const {}))\n",
        b.pool.heap_start()
    ));
    if b.unwind.is_some() {
        out.push_str("  (global (mut i32) (i32.const 0))\n");
        out.push_str("  (global (mut i32) (i32.const 0))\n");
        out.push_str("  (global (mut i64) (i64.const 0))\n");
    }
    let (offset, bytes) = b.pool.segment();
    out.push_str(&format!("  (data (i32.const {offset}) \""));
    for byte in bytes {
        out.push_str(&format!("\\{byte:02x}"));
    }
    out.push_str("\")\n");
    for f in &funcs {
        out.push_str(&func_wat(&f.params, &f.results, &f.locals, &f.body));
    }
    out.push_str(")\n");
    wat::parse_str(&out)
        .expect("the printed text is valid wasm text")
        .len()
}

/// The whole module, with the JSON set present or with every one of its
/// bodies replaced by a single `unreachable`, so the difference is what the
/// algorithms weigh rather than what a module weighs.
fn encoded(b: &Bases, whole: bool) -> usize {
    assert!(b.unwind.is_none(), "the size baseline is the plain module");
    let mut funcs = b.funcs();
    if !whole {
        let at = runtime::SET.len() + convert::SET.len();
        for f in funcs.iter_mut().skip(at) {
            f.body = vec![Ins::Unreachable];
            f.locals = Vec::new();
        }
    }
    let mut out = String::from("(module\n  (memory 1 200)\n");
    out.push_str(&format!(
        "  (global (mut i32) (i32.const {}))\n",
        b.pool.heap_start()
    ));
    let (offset, bytes) = b.pool.segment();
    out.push_str(&format!("  (data (i32.const {offset}) \""));
    for byte in bytes {
        out.push_str(&format!("\\{byte:02x}"));
    }
    out.push_str("\")\n");
    for f in &funcs {
        out.push_str(&func_wat(&f.params, &f.results, &f.locals, &f.body));
    }
    out.push_str(")\n");
    wat::parse_str(&out)
        .expect("the printed text is valid wasm text")
        .len()
}

// =========================================================================
// The corpora
// =========================================================================

fn valid_corpus() -> Vec<String> {
    let mut out: Vec<String> = vec![
        // literals
        "null",
        "true",
        "false",
        // numbers, every shape the JSONNumber grammar has
        "0",
        "1",
        "-1",
        "1.5",
        "-1.5",
        "0.5",
        "1e3",
        "1E3",
        "1e+3",
        "1e-3",
        "1.25e10",
        "-1.25e-10",
        "123456789012345678901234567890",
        "1e308",
        "1e-308",
        "5e-324",
        "1.7976931348623157e308",
        "0.1",
        "0.30000000000000004",
        "9007199254740993",
        // strings, every escape the JSONString grammar has
        r#""""#,
        r#""abc""#,
        r#""\"""#,
        r#""\\""#,
        r#""\/""#,
        r#""\b\f\n\r\t""#,
        r#""\u0000""#,
        r#""\u001f""#,
        r#""\u0041""#,
        r#""\u00e9""#,
        r#""\u4e2d\u6587""#,
        r#""\ud83d\ude00""#,
        r#""caf\u00e9 \u4e2d\u6587""#,
        "\"caf\u{e9}\"",
        "\"\u{1f600}\"",
        r#""a\u2028b\u2029c""#,
        // objects
        "{}",
        r#"{"a":1}"#,
        r#"{"a":1,"b":2,"c":3}"#,
        r#"{"a":{"b":{"c":{}}}}"#,
        r#"{"":0}"#,
        r#"{"a":null,"b":true,"c":false,"d":"e"}"#,
        r#"{"dup":1,"dup":2}"#,
        // whitespace, all four JSONWhiteSpace characters and nowhere else
        " \t\r\n{\t\"a\"\n:\r1 }\t",
        "\n  null  \n",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    // Deep nesting, one object per level.
    for depth in [1usize, 8, 40] {
        let mut text = String::new();
        for _ in 0..depth {
            text.push_str(r#"{"n":"#);
        }
        text.push('0');
        for _ in 0..depth {
            text.push('}');
        }
        out.push(text);
    }
    out
}

/// Every form the JSON grammar excludes, with the reason it is excluded. Each
/// row is checked against `serde_json` first, so a row that is secretly valid
/// fails loudly rather than passing quietly.
fn invalid_corpus() -> Vec<(String, &'static str)> {
    [
        ("", "empty text"),
        ("   ", "whitespace only"),
        ("{", "unterminated object"),
        ("}", "a close with no open"),
        (r#"{"a":1,}"#, "trailing comma in an object"),
        (r#"{"a":}"#, "member with no value"),
        (r#"{"a"}"#, "member with no colon"),
        (r#"{a:1}"#, "unquoted key"),
        (r#"{'a':1}"#, "single-quoted key"),
        (r#"{"a":1"b":2}"#, "member with no comma"),
        (r#"{,}"#, "leading comma"),
        ("'a'", "single-quoted string"),
        (r#""unterminated"#, "unterminated string"),
        ("\"raw\nnewline\"", "a raw control character in a string"),
        ("\"raw\ttab\"", "a raw tab in a string"),
        (r#""\x41""#, "an escape the grammar does not have"),
        (r#""\u12""#, "a short \\u escape"),
        (r#""\uZZZZ""#, "non-hex in a \\u escape"),
        ("+1", "a leading plus"),
        ("01", "a leading zero"),
        ("-01", "a signed leading zero"),
        (".5", "no integer part"),
        ("5.", "no fraction digits"),
        ("1e", "no exponent digits"),
        ("1e+", "a sign and no exponent digits"),
        ("0x10", "hexadecimal"),
        ("Infinity", "a numeric literal JSON does not have"),
        ("-Infinity", "a numeric literal JSON does not have"),
        ("NaN", "a numeric literal JSON does not have"),
        ("undefined", "a literal JSON does not have"),
        ("True", "the wrong case"),
        ("nul", "a truncated literal"),
        ("nulll", "a literal with a tail"),
        ("// a comment\n1", "a comment"),
        ("1 /* c */", "a comment"),
        ("1 2", "two values"),
        ("{} {}", "two values"),
        ("null null", "two values"),
        (r#""\ud800""#, "an unpaired leading surrogate"),
        (r#""\udc00""#, "an unpaired trailing surrogate"),
        (
            r#""\ud800\u0041""#,
            "a leading surrogate paired with a letter",
        ),
    ]
    .into_iter()
    .map(|(t, w)| (t.to_string(), w))
    .collect()
}

// =========================================================================
// The IR -> wasm text printer
// =========================================================================

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
        Ins::I32ShrU => "i32.shr_u".into(),
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
