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
