//! A property read off undefined/null/a primitive is a catchable TypeError
//! when the program has a `try` -- ECMA-262 13.3.2 / 7.1.18.
//!
//! Without a `try` there is no unwind channel and the read stays the named
//! fault (`FAULT_PROPERTY_OF_NON_OBJECT`); with one, `__obj_get` throws a
//! String and the call site's `throw_check` leaves for the handler.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{GuestFault, Value, compile_qjs_m1, guest_fault};

fn text(source: &str) -> String {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    match Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}")) {
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            let bytes: &[u8] = &view;
            let at = ptr as usize;
            let len = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
                as usize;
            String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8")
        }
        other => panic!("{source:?}: expected a String, got {other:?}"),
    }
}

#[test]
fn inside_a_try_the_read_is_a_catchable_type_error_naming_the_key() {
    assert_eq!(
        text(r#"try { let u = undefined; return u.a; } catch (e) { return e; }"#),
        "TypeError: cannot read property 'a' of a value that has no properties"
    );
    assert_eq!(
        text(r#"try { let n = null; return n.field; } catch (e) { return typeof e; }"#),
        "string"
    );
    assert_eq!(
        text(r#"let hit = "no"; try { let x = 1; x.y; } catch (e) { hit = "yes"; } return hit;"#),
        "yes"
    );
}

#[test]
fn a_finally_runs_and_the_script_continues_after_the_catch() {
    assert_eq!(
        text(
            r#"let log = ""; try { let u = undefined; u.a; } catch (e) { log = log + "c"; } finally { log = log + "f"; } return log + "!";"#
        ),
        "cf!"
    );
}

#[test]
fn without_a_try_the_read_is_still_the_named_fault() {
    let wasm = compile_qjs_m1(r#"let u = undefined; return u.a;"#).expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    assert!(instance.invoke_by_name("main", &Value::args(&[])).is_err());
    assert_eq!(
        guest_fault(&instance.memory().expect("guest memory")),
        Some(GuestFault::PropertyOfNonObject)
    );
}
