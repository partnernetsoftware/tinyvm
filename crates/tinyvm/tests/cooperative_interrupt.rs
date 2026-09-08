use core::sync::atomic::{AtomicBool, Ordering};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tinyvm::{Limits, Val, WasmError, WasmFaultClass, WasmModule};

fn must_ok<T>(result: Result<T, WasmError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {}", error.message()),
    }
}

fn start_loop_module(max_steps: u64) -> WasmModule {
    let bytes = wat::parse_str(
        r#"
        (module
          (func $spin (loop (br 0)))
          (start $spin))
        "#,
    )
    .expect("start-loop fixture must compile");
    must_ok(
        WasmModule::from_bytes_with(
            &bytes,
            Limits {
                max_steps,
                ..Limits::default()
            },
        ),
        "load start-loop fixture",
    )
}

#[test]
fn one_invocation_borrows_one_interrupt_identity() {
    let mut module = WasmModule::new_with_limits(Limits {
        max_steps: 4_096,
        ..Limits::default()
    });
    let spin = must_ok(
        module.add_function(0, 0, 0, &[0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b]),
        "add spin",
    );
    let answer = must_ok(
        module.add_function(0, 0, 1, &[0x41, 0x2a, 0x0b]),
        "add answer",
    );
    module.export("spin", spin);
    module.export("answer", answer);

    let mut instance = must_ok(module.instantiate(), "instantiate");
    let requested = AtomicBool::new(true);
    let interrupted = instance
        .invoke_by_name_with_interrupt("spin", &[], &requested)
        .expect_err("a set flag must interrupt the running invocation");
    assert_eq!(interrupted.message(), "interrupted");
    assert_eq!(interrupted.class(), WasmFaultClass::Interruption);
    assert!(interrupted.is_interrupted());
    assert_eq!(interrupted.ceiling(), None);
    assert_eq!(instance.last_steps(), 1_024);

    let fresh = AtomicBool::new(false);
    assert_eq!(
        must_ok(
            instance.invoke_by_name_with_interrupt("answer", &[], &fresh),
            "a fresh false flag must not inherit the previous interruption",
        ),
        vec![Val::I32(42)]
    );

    assert_eq!(
        instance.invoke(spin, &[]),
        Err(WasmError::Trap("step budget"))
    );
}

#[test]
fn running_pure_compute_observes_an_asynchronous_interrupt() {
    let mut module = WasmModule::new_with_limits(Limits {
        max_steps: u64::MAX,
        ..Limits::default()
    });
    let spin = must_ok(
        module.add_function(0, 0, 0, &[0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b]),
        "add spin",
    );
    module.export("spin", spin);

    let requested = Arc::new(AtomicBool::new(false));
    let requester = Arc::clone(&requested);
    let signal_after = Duration::from_millis(100);
    let helper = std::thread::spawn(move || {
        std::thread::sleep(signal_after);
        requester.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let interrupted = module
        .invoke_by_name_with_interrupt("spin", &[], requested.as_ref())
        .expect_err("the running loop must observe the asynchronous interrupt");
    let elapsed = started.elapsed();
    helper.join().expect("interrupt requester must not panic");

    assert!(interrupted.is_interrupted());
    assert!(
        elapsed >= signal_after && elapsed < signal_after + Duration::from_millis(150),
        "interrupt latency exceeded the precommitted bound: {elapsed:?}"
    );
}

#[test]
fn bulk_step_charges_cannot_jump_over_an_interrupt_poll() {
    let bytes = wat::parse_str(
        r#"
        (module
          (memory 1)
          (func (export "fill")
            (memory.fill (i32.const 0) (i32.const 0) (i32.const 16384))))
        "#,
    )
    .expect("bulk-memory fixture must compile");
    let module = must_ok(
        WasmModule::from_bytes_with(
            &bytes,
            Limits {
                max_steps: u64::MAX,
                ..Limits::default()
            },
        ),
        "load bulk-memory fixture",
    );
    let requested = AtomicBool::new(true);

    let interrupted = module
        .invoke_by_name_with_interrupt("fill", &[], &requested)
        .expect_err("a bulk charge that crosses the poll threshold must observe interruption");
    assert!(interrupted.is_interrupted());
}

#[test]
fn instance_start_borrows_the_instantiation_interrupt() {
    let module = start_loop_module(u64::MAX);
    let requested = AtomicBool::new(true);

    let interrupted = match module.instantiate_with_interrupt(&requested) {
        Ok(_) => panic!("a set flag must interrupt the start function"),
        Err(error) => error,
    };
    assert!(interrupted.is_interrupted());
}

#[test]
fn running_instance_start_observes_an_asynchronous_interrupt() {
    let module = start_loop_module(u64::MAX);
    let requested = Arc::new(AtomicBool::new(false));
    let requester = Arc::clone(&requested);
    let signal_after = Duration::from_millis(100);
    let helper = std::thread::spawn(move || {
        std::thread::sleep(signal_after);
        requester.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let interrupted = match module.instantiate_with_interrupt(requested.as_ref()) {
        Ok(_) => panic!("the running start function must observe interruption"),
        Err(error) => error,
    };
    let elapsed = started.elapsed();
    helper.join().expect("interrupt requester must not panic");

    assert!(interrupted.is_interrupted());
    assert!(
        elapsed >= signal_after && elapsed < signal_after + Duration::from_millis(150),
        "start interruption exceeded the precommitted bound: {elapsed:?}"
    );
}

#[test]
fn instantiation_interrupt_is_not_retained_by_the_instance() {
    let mut module = WasmModule::new();
    module.add_global(Val::I32(0), true);
    let start = must_ok(
        module.add_function(0, 0, 0, &[0x23, 0x00, 0x41, 0x01, 0x6a, 0x24, 0x00, 0x0b]),
        "add finite start",
    );
    let read = must_ok(
        module.add_function(0, 0, 1, &[0x23, 0x00, 0x0b]),
        "add global reader",
    );
    module.set_start(start);
    module.export("read", read);
    let requested = AtomicBool::new(false);
    let mut instance = must_ok(
        module.instantiate_with_interrupt(&requested),
        "instantiate with a fresh flag",
    );

    requested.store(true, Ordering::Relaxed);
    assert_eq!(
        must_ok(
            instance.invoke_by_name("read", &[]),
            "ordinary calls must not retain the instantiation flag",
        ),
        vec![Val::I32(1)]
    );
}

#[test]
fn ordinary_instantiation_keeps_the_existing_step_budget() {
    let module = start_loop_module(1_024);

    let error = match module.instantiate() {
        Ok(_) => panic!("the legacy API must remain bounded by max_steps"),
        Err(error) => error,
    };
    assert_eq!(error, WasmError::Trap("step budget"));
}
