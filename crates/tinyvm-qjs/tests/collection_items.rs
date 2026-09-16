//! One runtime ceiling at every Array creation/growth path.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{
    COLLECTION_ITEMS_LIMIT_IMPORT, GuestFault, Options, RUNTIME_LIMIT_MODULE, RuntimeLimits, Value,
    compile_qjs_m1_with_runtime_limits, guest_fault,
};

fn compile(source: &str) -> Vec<u8> {
    compile_qjs_m1_with_runtime_limits(
        source,
        Options::default(),
        RuntimeLimits {
            collection_items: true,
        },
    )
    .expect("source compiles")
}

fn instantiate(bytes: &[u8], limit: i32) -> WasmInstance {
    let mut module = WasmModule::from_bytes_with(bytes, Limits::default()).expect("loads");
    assert!(module.imports().iter().any(|import| {
        import.module == RUNTIME_LIMIT_MODULE && import.field == COLLECTION_ITEMS_LIMIT_IMPORT
    }));
    module
        .bind_import_typed(
            RUNTIME_LIMIT_MODULE,
            COLLECTION_ITEMS_LIMIT_IMPORT,
            move |args, _memory| {
                assert!(args.is_empty());
                Ok(vec![Val::I32(limit)])
            },
        )
        .expect("runtime limit binds");
    module.instantiate().expect("instantiates")
}

fn run(bytes: &[u8], limit: i32) -> Result<Value, GuestFault> {
    let mut instance = instantiate(bytes, limit);
    match instance.invoke_by_name("main", &Value::args(&[])) {
        Ok(values) => Ok(Value::returned(&values).expect("returns one JS value")),
        Err(_) => Err(guest_fault(&instance.memory().expect("memory zero"))
            .expect("a limit refusal records its reason")),
    }
}

fn exact_then_plus_one(source: &str, exact: i32) {
    let bytes = compile(source);
    assert!(
        run(&bytes, exact).is_ok(),
        "exact limit must pass: {source}"
    );
    assert_eq!(
        run(&bytes, exact - 1),
        Err(GuestFault::CollectionItemsExhausted),
        "limit plus one must refuse: {source}"
    );
}

#[test]
fn every_array_construction_path_uses_the_same_runtime_ceiling() {
    for source in [
        "return [1, 2, 3].length;",
        "let a = []; a.push(1); a.push(2); a.push(3); return a.length;",
        "let a = []; a[2] = 3; return a.length;",
        "return [1].concat([2, 3]).length;",
        "let f = function (x) { return x + 1; }; return [1, 2, 3].map(f).length;",
        "return JSON.parse(\"[1,2,3]\").length;",
    ] {
        exact_then_plus_one(source, 3);
    }
}

#[test]
fn one_packed_artifact_accepts_a_different_ceiling_each_time_it_is_loaded() {
    let bytes = compile("return [1, 2, 3].length;");
    assert_eq!(run(&bytes, 2), Err(GuestFault::CollectionItemsExhausted));
    assert!(run(&bytes, 3).is_ok());
}

#[test]
fn collections_are_bounded_individually_not_cumulatively() {
    let bytes = compile("let a = [1, 2]; let b = [3, 4]; return a.length + b.length;");
    assert!(run(&bytes, 2).is_ok());
}

#[test]
fn the_runtime_import_is_opt_in_and_array_gated() {
    let ordinary = tinyvm_qjs::compile_qjs_m1("return [1];").expect("ordinary source compiles");
    let ordinary = WasmModule::from_bytes_with(&ordinary, Limits::default()).expect("loads");
    assert!(!ordinary.imports().iter().any(|import| {
        import.module == RUNTIME_LIMIT_MODULE && import.field == COLLECTION_ITEMS_LIMIT_IMPORT
    }));

    let scalar = compile_qjs_m1_with_runtime_limits(
        "return 1;",
        Options::default(),
        RuntimeLimits {
            collection_items: true,
        },
    )
    .expect("scalar source compiles");
    let scalar = WasmModule::from_bytes_with(&scalar, Limits::default()).expect("loads");
    assert!(!scalar.imports().iter().any(|import| {
        import.module == RUNTIME_LIMIT_MODULE && import.field == COLLECTION_ITEMS_LIMIT_IMPORT
    }));
}
