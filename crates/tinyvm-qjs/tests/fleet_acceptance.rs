//! The acceptance target, compiled and then **run**.
//!
//! `agenterm/scripts/qjs/lib/fleet.js` is the library this milestone series
//! exists to compile: 231 lines that wrap one host call in a tree of namespace
//! tables. Compiling it is one claim and `tests/function_values.rs` holds it.
//! This file holds the other one, which is the one a downstream embedder
//! actually needs: that the wrapper *works* -- a JavaScript call goes out
//! through a raw host door, a broker answers with JSON text, `JSON.parse`
//! turns it into an Object, and the caller reads a property off it.
//!
//! # Why the driver is reduced, and what exactly is reduced about it
//!
//! `fleet.js` reaches its door as `__host.fleet_call(op, params)` -- a
//! property call on a **free name**. Under [`Names::HostImport`] a free name is
//! a zero-argument `js.*` import answering one V1 pair, and no host can answer
//! that pair with an Object: [`tinyvm_qjs::Value`] has no Object variant, and
//! building an object record in guest memory by hand would mean the host
//! knowing this engine's record layout, which is exactly the leak the raw door
//! exists to prevent (see the README's *Reaching a host, with arguments*).
//!
//! So the embedder supplies `__host` itself, in five lines of JavaScript
//! prepended to the library:
//!
//! ```js
//! const __host = { fleet_call: function (op, p) { return fleet_result_of(op, p); } };
//! function fleet_result_of(op, p) { fleet_call(op, p); return fleet_result(); }
//! ```
//!
//! -- where `fleet_call` and `fleet_result` are two [`Names::Declared`] raw
//! doors, the second a `Bytes` result so the answer comes back as a String.
//! Two and not one because the raw contract is a status code plus a two-pass
//! read, which is the shape a variable-length host answer has and not
//! something this wrapper invented.
//!
//! That is the whole of the reduction. Everything below the prelude is
//! `fleet.js`'s own text, copied rather than paraphrased: the `call()` helper
//! verbatim, and wrappers spelled exactly as the library spells them.
//!
//! What this file is therefore *not*: a claim that `fleet.js` runs unmodified.
//! It does not, and the missing piece is named above rather than glossed.

use std::cell::RefCell;
use std::rc::Rc;

use tinyvm::{Limits, Val, WasmError, WasmInstance, WasmModule};
use tinyvm_qjs::{
    HostFn, HostParam, HostResult, Names, Options, Value, compile_qjs_m1_with, guest_fault,
};

// =========================================================================
// The host door, and a broker behind it
// =========================================================================

/// ```text
/// fleet.call(op_ptr, op_len, params_ptr, params_len) -> i32
/// fleet.result_len() -> i32
/// fleet.result(dst_ptr, dst_cap) -> i32
/// ```
///
/// Two declarations, three imports: a `Bytes` result is the two-pass read, so
/// it takes a length import beside the copy. The vocabulary is the embedder's
/// -- this crate names nobody's host function -- so these are the names an
/// AgenTerm-shaped embedder would pick, and nothing in `src/` knows them.
fn door() -> Vec<HostFn> {
    vec![
        HostFn {
            name: "fleet_call".to_string(),
            module: "fleet".to_string(),
            field: "call".to_string(),
            params: vec![HostParam::StrPtrLen, HostParam::StrPtrLen],
            result: HostResult::I32,
        },
        HostFn {
            name: "fleet_result".to_string(),
            module: "fleet".to_string(),
            field: "result".to_string(),
            params: Vec::new(),
            result: HostResult::Bytes {
                length: "result_len".to_string(),
            },
        },
    ]
}

/// A stand-in Fleet broker: it records what it was asked and answers with the
/// text the real one would.
#[derive(Debug, Default)]
struct Broker {
    /// Every `(operation_id, params_json)` pair the script sent.
    asked: Vec<(String, String)>,
    /// The text the next `fleet_result()` hands back.
    answer: String,
}

/// The prelude plus `fleet.js`'s own `call()`, exactly as the library writes
/// it -- the conditional, the `try`, the `catch` that falls back to the raw
/// text. Copied from `agenterm/scripts/qjs/lib/fleet.js` lines 12 to 21.
const CALL: &str = r#"
const __host = { fleet_call: function (op, p) { return fleet_result_of(op, p); } };

function fleet_result_of(op, p) {
  fleet_call(op, p);
  return fleet_result();
}

function call(opId, params) {
  const resultJson = __host.fleet_call(opId, params === undefined ? "{}" : params);
  try {
    return JSON.parse(resultJson);
  } catch (_err) {
    return resultJson;
  }
}
"#;

// =========================================================================
// Harness
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
    Object,
}

const TAG_OBJECT: i32 = 5;

#[track_caller]
fn drive(source: &str, broker: &Rc<RefCell<Broker>>) -> Out {
    let wasm = compile_qjs_m1_with(
        source,
        Options {
            names: Names::Declared(door()),
        },
    )
    .unwrap_or_else(|e| panic!("compiling the driver: {}", e.message));
    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected the driver: {}", e.message()));

    let seen = Rc::clone(broker);
    module
        .bind_import_typed("fleet", "call", move |args, memory| {
            let [Val::I32(op), Val::I32(op_len), Val::I32(p), Val::I32(p_len)] = args else {
                return Err(WasmError::Trap("fleet.call wants four i32"));
            };
            let text = |at: i32, len: i32| {
                String::from_utf8(memory[at as usize..(at + len) as usize].to_vec())
                    .expect("the guest hands over UTF-8")
            };
            seen.borrow_mut()
                .asked
                .push((text(*op, *op_len), text(*p, *p_len)));
            Ok(vec![Val::I32(0)])
        })
        .expect("bind fleet.call");
    let seen = Rc::clone(broker);
    module
        .bind_import_typed("fleet", "result_len", move |_args, _memory| {
            Ok(vec![Val::I32(seen.borrow().answer.len() as i32)])
        })
        .expect("bind fleet.result_len");
    let seen = Rc::clone(broker);
    module
        .bind_import_typed("fleet", "result", move |args, memory| {
            let [Val::I32(dst), Val::I32(cap)] = args else {
                return Err(WasmError::Trap("fleet.result wants (i32, i32)"));
            };
            let bytes = seen.borrow().answer.clone().into_bytes();
            if bytes.len() as i32 > *cap {
                return Ok(vec![Val::I32(-1)]);
            }
            let at = *dst as usize;
            memory[at..at + bytes.len()].copy_from_slice(&bytes);
            Ok(vec![Val::I32(bytes.len() as i32)])
        })
        .expect("bind fleet.result");

    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating the driver: {}", e.message()));
    let vals = instance.invoke_by_name("main", &[]).unwrap_or_else(|e| {
        let fault = instance.memory().ok().and_then(|m| guest_fault(&m));
        panic!("trap in the driver: {} (fault {fault:?})", e.message())
    });
    if let [Val::I32(TAG_OBJECT), _] = vals[..] {
        return Out::Object;
    }
    match Value::returned(&vals).expect("a readable result") {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => Out::Str(read_string(&instance, ptr)),
    }
}

fn read_string(instance: &WasmInstance, ptr: i32) -> String {
    let view = instance.memory().expect("guest memory");
    let at = ptr as usize;
    let len = u32::from_le_bytes([view[at], view[at + 1], view[at + 2], view[at + 3]]) as usize;
    String::from_utf8(view[at + 4..at + 4 + len].to_vec()).expect("UTF-8")
}

/// A broker primed with one answer.
fn primed(answer: &str) -> Rc<RefCell<Broker>> {
    Rc::new(RefCell::new(Broker {
        asked: Vec::new(),
        answer: answer.to_string(),
    }))
}

// =========================================================================
// The wrappers, end to end
// =========================================================================

/// `fleet.tabs.set_note(tabId, note)`, spelled exactly as the library spells
/// it, driven all the way to a property of the parsed answer.
///
/// Six things have to be right at once for the last line to read `true`, and
/// each of them is a capability this series added: the object literal in the
/// prelude, the function value in the property, the call through it, the
/// conditional in `call()`, `JSON.stringify` on the way out, and `JSON.parse`
/// on the way back.
#[test]
fn a_fleet_wrapper_runs_end_to_end_through_the_host_door() {
    let broker = primed(r#"{"ok":true,"tab":"t3","note":"ship it"}"#);
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.tabs = {{}};
        fleet.tabs.set_note = function (tabId, note) {{
          return call(\"tabs.set-note\", JSON.stringify({{ tab: tabId, note: note }}));
        }};
        return fleet.tabs.set_note(\"t3\", \"ship it\").ok;"
    );
    assert_eq!(drive(&source, &broker), Out::Bool(true));

    // What the host actually saw. The params key is `tab` and not `tab_id`,
    // which is the whole point of the wrapper -- and the JSON that carried it
    // was written by this engine.
    let asked = broker.borrow().asked.clone();
    assert_eq!(
        asked,
        [(
            "tabs.set-note".to_string(),
            r#"{"tab":"t3","note":"ship it"}"#.to_string()
        )],
        "the operation and its params, as the broker received them"
    );
}

/// The zero-argument spelling, which is what twenty-two of the library's
/// twenty-nine methods are: `call(op)` with no params, so `params` is
/// `undefined` and the conditional supplies `"{}"`.
#[test]
fn a_zero_argument_wrapper_sends_the_empty_object() {
    let broker = primed(r#"{"tab":"t7","title":"main"}"#);
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.tabs = {{}};
        fleet.tabs.active = function () {{ return call(\"tabs.active\"); }};
        return fleet.tabs.active().tab;"
    );
    assert_eq!(drive(&source, &broker), Out::Str("t7".into()));
    assert_eq!(
        broker.borrow().asked,
        [("tabs.active".to_string(), "{}".to_string())],
        "the conditional supplied the default"
    );
}

/// The `catch`, on the answer that made the library need one: a broker error
/// is not JSON, so the raw text comes back and the caller sees a String.
#[test]
fn a_non_json_answer_takes_the_catch_and_comes_back_as_text() {
    let broker = primed("broker_invalid_arguments: tabs.set-note does not accept parameter tab_id");
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.tabs = {{}};
        fleet.tabs.active = function () {{ return call(\"tabs.active\"); }};
        return fleet.tabs.active();"
    );
    assert_eq!(
        drive(&source, &broker),
        Out::Str("broker_invalid_arguments: tabs.set-note does not accept parameter tab_id".into())
    );
}

/// A JSON **array** in the answer is a list the caller can index.
///
/// This test used to be `an_array_answer_comes_back_as_text_because_there_is_no_array_type`,
/// and it asserted that `tabs.list` -- the most obviously useful operation in
/// the catalog -- took the binding's `catch` and handed the caller the raw
/// text. It is written here, in the file about the product, because nowhere
/// else said what that cost the caller; the Array milestone is what changed
/// the answer, and this is the same shape asserting the new one.
#[test]
fn an_array_answer_is_a_list_the_caller_can_index() {
    let broker = primed(r#"[{"tab":"t1"},{"tab":"t2"}]"#);
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.tabs = {{}};
        fleet.tabs.list = function () {{ return call(\"tabs.list\"); }};
        const tabs = fleet.tabs.list();
        return tabs.length + \"/\" + tabs[0].tab + \"/\" + tabs[1].tab;"
    );
    assert_eq!(drive(&source, &broker), Out::Str("2/t1/t2".into()));
}

/// Three wrappers in one module, called in turn, with the broker answering
/// differently each time -- so the namespace tree, the parsed Objects and the
/// bump heap all survive more than one round trip.
#[test]
fn three_round_trips_in_one_call_keep_their_answers_apart() {
    let broker = primed(r#"{"n":1}"#);
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.ui = {{}};
        fleet.ui.tabs = {{}};
        fleet.ui.tabs.set_width = function (width) {{
          return call(\"ui.tabs.set-width\", JSON.stringify({{ width: width }}));
        }};
        fleet.ui.input = {{}};
        fleet.ui.input.pointer = function (x, y, action) {{
          return call(\"ui.input.pointer\", JSON.stringify({{ x: x, y: y, action: action }}));
        }};
        const a = fleet.ui.tabs.set_width(40);
        const b = fleet.ui.input.pointer(3, 4, \"down\");
        const c = fleet.ui.tabs.set_width(41);
        return a.n + b.n + c.n;"
    );
    assert_eq!(drive(&source, &broker), Out::Number(3.0));
    assert_eq!(
        broker.borrow().asked,
        [
            (
                "ui.tabs.set-width".to_string(),
                r#"{"width":40}"#.to_string()
            ),
            (
                "ui.input.pointer".to_string(),
                r#"{"x":3,"y":4,"action":"down"}"#.to_string()
            ),
            (
                "ui.tabs.set-width".to_string(),
                r#"{"width":41}"#.to_string()
            ),
        ]
    );
}

/// The parsed answer is an ordinary Object of this engine's, which is what
/// says `JSON.parse` produced a value and not a description of one.
#[test]
fn the_parsed_answer_is_an_ordinary_object() {
    let broker = primed(r#"{"a":{"b":{"c":7}}}"#);
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.probe = function () {{ return call(\"probe\"); }};
        const answer = fleet.probe();
        return typeof answer;"
    );
    assert_eq!(drive(&source, &broker), Out::Str("object".into()));

    let broker = primed(r#"{"a":{"b":{"c":7}}}"#);
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.probe = function () {{ return call(\"probe\"); }};
        return fleet.probe().a.b.c;"
    );
    assert_eq!(drive(&source, &broker), Out::Number(7.0));

    let broker = primed(r#"{"a":1}"#);
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.probe = function () {{ return call(\"probe\"); }};
        return fleet.probe();"
    );
    assert_eq!(drive(&source, &broker), Out::Object);
}

/// The instance is reused, which is what the downstream slot does: one
/// `WasmInstance` per slot and one `invoke_by_name` per invocation.
///
/// This is the shape the stale-flag defect was found in -- an uncaught throw
/// left the in-flight flag raised and the *next* call's `catch` fired on it.
/// Here the throw is real and internal (`JSON.parse` on text that is not
/// JSON), it is caught, and three calls in a row have to agree.
#[test]
fn one_instance_answers_three_calls_the_same_way() {
    let answers = Rc::new(RefCell::new(vec![
        r#"{"ok":true}"#.to_string(),
        "not json at all".to_string(),
        r#"{"ok":true}"#.to_string(),
    ]));
    let source = format!(
        "{CALL}
        const fleet = {{}};
        fleet.probe = function () {{ return call(\"probe\"); }};
        return typeof fleet.probe();"
    );
    let wasm = compile_qjs_m1_with(
        &source,
        Options {
            names: Names::Declared(door()),
        },
    )
    .expect("compiles");
    let mut module =
        WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the gate");
    module
        .bind_import_typed("fleet", "call", |_args, _memory| Ok(vec![Val::I32(0)]))
        .expect("bind");
    let seen = Rc::clone(&answers);
    module
        .bind_import_typed("fleet", "result_len", move |_args, _memory| {
            Ok(vec![Val::I32(
                seen.borrow().first().map_or(0, |a| a.len()) as i32
            )])
        })
        .expect("bind");
    let seen = Rc::clone(&answers);
    module
        .bind_import_typed("fleet", "result", move |args, memory| {
            let [Val::I32(dst), Val::I32(cap)] = args else {
                return Err(WasmError::Trap("wants (i32, i32)"));
            };
            let mut queue = seen.borrow_mut();
            let bytes = if queue.is_empty() {
                Vec::new()
            } else {
                queue.remove(0).into_bytes()
            };
            if bytes.len() as i32 > *cap {
                return Ok(vec![Val::I32(-1)]);
            }
            let at = *dst as usize;
            memory[at..at + bytes.len()].copy_from_slice(&bytes);
            Ok(vec![Val::I32(bytes.len() as i32)])
        })
        .expect("bind");
    let mut instance = module.instantiate().expect("instantiates");

    // Object, then String (the `catch`), then Object again. The third answer
    // is the one the stale flag used to corrupt.
    for want in ["object", "string", "object"] {
        let vals = instance
            .invoke_by_name("main", &[])
            .unwrap_or_else(|e| panic!("trap: {}", e.message()));
        let Ok(Value::String(ptr)) = Value::returned(&vals) else {
            panic!("`typeof` must answer with a String");
        };
        assert_eq!(read_string(&instance, ptr), want);
    }
}
