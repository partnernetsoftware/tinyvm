# tinyvm in-guest interpreter throughput

Owner: [tinyvm PRD](../prd/PRD.md)

Status: executable development evidence; timings are not release thresholds

The [cross-boundary benchmark](tinyvm-boundary-benchmark.md) measures what it
costs to *enter* the guest. This gate measures what it costs to *stay* there:
the per-instruction dispatch cost of the defined-function interpreter. That is
the half of the product sentence — write once, many platforms, **high
performance** — that no other gate covered.

## What is measured

`smoke-interpreter-throughput.sh` compiles eight fixtures with `wat2wasm`,
validates each one, and runs each through WABT's independent interpreter
first. Only bytes WABT has already agreed with are handed to the timed tinyvm
run, and tinyvm must return the same value again through a host oracle that
recomputes the answer in Rust. Timing a wrong answer is the failure mode this
ordering exists to prevent.

The denominator is tinyvm's own step counter, not a hand count. Each row
reports `last_steps()` retired guest instructions alongside elapsed
nanoseconds, so the headline number is **nanoseconds per guest instruction on a
fixed instruction mix**. A dispatch change stays visible even when a workload's
absolute runtime moves for an unrelated reason, and two machines stay
comparable.

Every fixture exports a zero-argument `run` that loops a baked-in 20,000 trips,
which is the shape WABT's `--run-all-exports` can drive with no harness. The
test re-enters the guest 50 times by default, so boundary cost is amortised
across hundreds of thousands of in-guest steps per call rather than measured
here.

## The eight workload shapes

| Workload | Stresses |
| --- | --- |
| `i32_loop` | dispatch + i32 ALU — the dispatch floor, undiluted |
| `i64_loop` | dispatch + i64 ALU — separates wide values from slow dispatch |
| `f64_math` | f64 ALU + conversions — a `no_std` build routes some through libm |
| `memory_scan` | i32 load/store bounds checks against the live memory slot |
| `call_direct` | activation setup, argument moves and the return path |
| `call_indirect` | table bounds, element liveness and the run-time type check |
| `br_table` | the control stack, not the operand stack |
| `local_shuffle` | local indexing in a wide frame |

The gate is the matrix, not the ranking: eight shapes, one positive observation
each, same answers from both engines. A silently skipped row cannot pass as a
fast one.

## Baseline, 2026-08-22

Release profile, Apple Silicon development machine, 50 repetitions. These are
the numbers the interpreter had on the day the gate was written, recorded so a
later change has something to be measured against.

| Workload | ns / guest instruction | M steps/s |
| --- | ---: | ---: |
| `i32_loop` | 8.655 | 115.5 |
| `i64_loop` | 7.302 | 136.9 |
| `f64_math` | 8.130 | 123.0 |
| `memory_scan` | 8.289 | 120.6 |
| `call_direct` | 17.907 | 55.8 |
| `call_indirect` | 18.795 | 53.2 |
| `br_table` | 8.273 | 120.9 |
| `local_shuffle` | 6.380 | 156.7 |

Read the shape, not the absolute values. Straight-line work sat near 8 ns per
instruction while call-heavy work sat near 18 ns, on a machine where a
comparable no-JIT interpreter reaches low single-digit nanoseconds. That gap
was the reason the per-instruction prologue became the first optimization
target rather than any individual opcode.

## Running it

```sh
crates/tinyvm/smoke-interpreter-throughput.sh
```

Needs `wat2wasm`, `wasm-validate` and `wasm-interp` from WABT. Without WABT the
timed half still runs standalone, assembling the fixtures in process and
keeping the host oracle:

```sh
cargo test --release -p tinyvm --test interpreter_throughput -- --ignored --nocapture
```

`TINYVM_THROUGHPUT_REPETITIONS` scales the entry count on a slow machine. It
changes how long the run takes, not the instruction mix.
