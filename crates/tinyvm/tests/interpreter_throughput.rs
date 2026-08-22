//! Development-only in-guest interpreter throughput benchmark.
//!
//! The cross-boundary benchmark measures what it costs to *enter* the guest.
//! This one measures what it costs to *stay* there: the per-instruction
//! dispatch cost of the defined-function interpreter, which is the part of the
//! product sentence ("write once, many platforms, high performance") that no
//! other gate covers.
//!
//! Elapsed time is evidence, not a deterministic correctness gate, so this is
//! ignored in ordinary tests. Run it through `smoke-interpreter-throughput.sh`,
//! which compiles the same fixtures with WABT and requires its independent
//! interpreter to produce the same answers before any timing is believed.
//!
//! The denominator is tinyvm's own step counter, not a hand count: every row
//! reports `last_steps()` retired guest instructions, so the headline number is
//! nanoseconds per guest instruction on a fixed instruction mix. A dispatch
//! change stays visible even when a workload's absolute runtime moves for an
//! unrelated reason, and two machines stay comparable.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use tinyvm::{Limits, Val, WasmError, WasmModule};

/// Trip count baked into every fixture's loop. The host oracles below
/// recompute against this same number, so editing a fixture's constant without
/// editing this one fails the row instead of silently changing the mix.
const FIXTURE_TRIPS: i32 = 20_000;

fn must_ok<T>(result: Result<T, WasmError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {}", error.message()),
    }
}

fn only_i32(values: &[Val]) -> i32 {
    match values {
        [Val::I32(value)] => *value,
        other => panic!(
            "expected exactly one i32 result, got {} values",
            other.len()
        ),
    }
}

/// Times the whole workload is re-entered. Each entry runs `FIXTURE_TRIPS`
/// trips in-guest, so boundary cost is amortised across hundreds of thousands
/// of in-guest steps per call rather than measured here.
fn repetitions() -> usize {
    std::env::var("TINYVM_THROUGHPUT_REPETITIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(50)
        .max(3)
}

/// A workload is one fixture exporting a zero-argument `run`, which is the
/// shape WABT's `--run-all-exports` can drive without a harness.
struct Workload {
    name: &'static str,
    /// What the instruction mix is meant to stress, printed with the row so a
    /// regression points at a mechanism rather than at a name.
    stresses: &'static str,
    fixture: &'static str,
    /// Independent host recomputation of the guest's answer. A wrong result
    /// fails the row instead of publishing a fast but meaningless timing.
    expect: fn() -> i32,
}

const WORKLOADS: &[Workload] = &[
    Workload {
        name: "i32_loop",
        stresses: "dispatch + i32 ALU",
        fixture: "throughput-i32-loop-v1",
        expect: i32_loop_expect,
    },
    Workload {
        name: "i64_loop",
        stresses: "dispatch + i64 ALU",
        fixture: "throughput-i64-loop-v1",
        expect: i64_loop_expect,
    },
    Workload {
        name: "f64_math",
        stresses: "f64 ALU + conversions",
        fixture: "throughput-f64-math-v1",
        expect: f64_math_expect,
    },
    Workload {
        name: "memory_scan",
        stresses: "i32 load/store bounds checks",
        fixture: "throughput-memory-scan-v1",
        expect: memory_scan_expect,
    },
    Workload {
        name: "call_direct",
        stresses: "activation setup + return",
        fixture: "throughput-call-direct-v1",
        expect: call_direct_expect,
    },
    Workload {
        name: "call_indirect",
        stresses: "table dispatch + type check",
        fixture: "throughput-call-indirect-v1",
        expect: call_indirect_expect,
    },
    Workload {
        name: "br_table",
        stresses: "control stack + branch tables",
        fixture: "throughput-br-table-v1",
        expect: br_table_expect,
    },
    Workload {
        name: "local_shuffle",
        stresses: "local indexing in a wide frame",
        fixture: "throughput-local-shuffle-v1",
        expect: local_shuffle_expect,
    },
];

fn i32_loop_expect() -> i32 {
    let mut acc: i32 = 0;
    for index in 0..FIXTURE_TRIPS {
        acc = acc.wrapping_add(index.wrapping_mul(3));
        acc ^= ((acc as u32) >> 7) as i32;
    }
    acc
}

fn i64_loop_expect() -> i32 {
    let mut acc: i64 = 0;
    for index in 0..FIXTURE_TRIPS {
        acc = acc.wrapping_add(i64::from(index).wrapping_mul(3));
        acc ^= ((acc as u64) >> 7) as i64;
    }
    acc as i32
}

fn f64_math_expect() -> i32 {
    let mut acc: f64 = 1.0;
    for _ in 0..FIXTURE_TRIPS {
        acc = acc * 1.000_000_1 + 0.5;
        acc -= (acc * 0.5).floor();
    }
    // i32.trunc_sat_f64_s
    if acc.is_nan() {
        0
    } else if acc >= f64::from(i32::MAX) {
        i32::MAX
    } else if acc <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        acc as i32
    }
}

fn memory_scan_expect() -> i32 {
    let mut acc: i32 = 0;
    for index in 0..FIXTURE_TRIPS {
        let stored = index.wrapping_add(7);
        acc = acc.wrapping_add(stored);
        // `i32.load8_u` reads the low little-endian byte of the value just
        // stored, zero-extended to i32.
        acc ^= (stored as u32 & 0xff) as i32;
    }
    acc
}

fn call_direct_expect() -> i32 {
    FIXTURE_TRIPS
}

fn call_indirect_expect() -> i32 {
    let mut acc: i32 = 0;
    for index in 0..FIXTURE_TRIPS {
        acc = if index & 1 == 0 {
            acc.wrapping_add(3)
        } else {
            acc.wrapping_sub(1)
        };
    }
    acc
}

fn br_table_expect() -> i32 {
    let mut acc: i32 = 0;
    for index in 0..FIXTURE_TRIPS {
        acc = acc.wrapping_add(match (index as u32) % 3 {
            0 => 1 + 2 + 4,
            1 => 2 + 4,
            _ => 4,
        });
    }
    acc
}

fn local_shuffle_expect() -> i32 {
    let mut slots = [0i32; 7];
    for index in 0..FIXTURE_TRIPS {
        slots[6] = slots[5];
        slots[5] = slots[4];
        slots[4] = slots[3];
        slots[3] = slots[2];
        slots[2] = slots[1];
        slots[1] = slots[0];
        slots[0] = slots[6].wrapping_add(index);
    }
    slots[0]
}

/// Module bytes for one workload.
///
/// `smoke-interpreter-throughput.sh` sets `TINYVM_THROUGHPUT_WASM_DIR` to a
/// directory of WABT-compiled modules, so the timed engine runs the exact bytes
/// the independent interpreter just agreed with. Without it the fixture is
/// assembled in process, which keeps a bare `cargo test -- --ignored` useful on
/// a machine with no WABT installed.
fn workload_bytes(workload: &Workload) -> Vec<u8> {
    if let Ok(dir) = std::env::var("TINYVM_THROUGHPUT_WASM_DIR") {
        let path = PathBuf::from(dir).join(format!("{}.wasm", workload.fixture));
        return std::fs::read(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{}.wat", workload.fixture));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    wat::parse_str(&text).unwrap_or_else(|error| panic!("assemble {}: {error}", path.display()))
}

/// One measured row. Steps come from the engine, so ns/step is a ratio of two
/// measured quantities rather than an assumed instruction count.
struct Observation {
    name: &'static str,
    stresses: &'static str,
    steps: u64,
    nanos: u128,
}

impl Observation {
    fn nanos_per_step(&self) -> f64 {
        self.nanos as f64 / self.steps as f64
    }

    fn million_steps_per_second(&self) -> f64 {
        if self.nanos == 0 {
            return 0.0;
        }
        self.steps as f64 * 1_000.0 / self.nanos as f64
    }
}

fn measure(workload: &Workload, repetitions: usize) -> Observation {
    let wasm = workload_bytes(workload);
    // The step budget is a host ceiling, not a language limit; a benchmark
    // deliberately runs past the product default.
    let limits = Limits {
        max_steps: u64::MAX,
        ..Limits::default()
    };
    let module = must_ok(
        WasmModule::from_bytes_with(&wasm, limits),
        "load benchmark module",
    );
    let mut instance = must_ok(module.instantiate(), "instantiate benchmark module");
    let expected = (workload.expect)();

    // One untimed entry: page in the memory slot and let the branch predictor
    // see the loop before the clock starts.
    let warm = must_ok(instance.invoke_by_name("run", &[]), "warm benchmark call");
    assert_eq!(
        only_i32(&warm),
        expected,
        "{} warm result must match the host oracle",
        workload.name
    );

    let mut steps = 0u64;
    let start = Instant::now();
    for _ in 0..repetitions {
        let values = must_ok(instance.invoke_by_name("run", &[]), "timed benchmark call");
        black_box(&values);
        steps += instance.last_steps();
    }
    let nanos = start.elapsed().as_nanos();

    // Re-check after timing: a mid-run miscompare would otherwise be published
    // as a fast row.
    let after = must_ok(instance.invoke_by_name("run", &[]), "verify benchmark call");
    assert_eq!(
        only_i32(&after),
        expected,
        "{} result must match the host oracle after timing",
        workload.name
    );
    assert!(steps > 0, "{} must retire guest steps", workload.name);

    Observation {
        name: workload.name,
        stresses: workload.stresses,
        steps,
        nanos,
    }
}

#[test]
#[ignore = "development benchmark; run through smoke-interpreter-throughput.sh"]
fn interpreter_throughput_reports_nanoseconds_per_guest_instruction() {
    let repetitions = repetitions();
    println!("engine,workload,stresses,steps,nanos,nanos_per_step,million_steps_per_second");
    let mut observations = Vec::new();
    for workload in WORKLOADS {
        let observation = measure(workload, repetitions);
        println!(
            "tinyvm,{},{},{},{},{:.3},{:.1}",
            observation.name,
            observation.stresses,
            observation.steps,
            observation.nanos,
            observation.nanos_per_step(),
            observation.million_steps_per_second(),
        );
        observations.push(observation);
    }

    // The matrix, not the ranking, is the gate: every workload shape must have
    // produced a positive observation, so a silently skipped row cannot pass as
    // a fast one.
    assert_eq!(
        observations.len(),
        WORKLOADS.len(),
        "every workload shape must publish one observation"
    );
    for observation in &observations {
        assert!(
            observation.nanos > 0,
            "{} must record elapsed time",
            observation.name
        );
        assert!(
            observation.nanos_per_step().is_finite() && observation.nanos_per_step() > 0.0,
            "{} must record a positive per-step cost",
            observation.name
        );
    }
}
