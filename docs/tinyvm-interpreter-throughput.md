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

## Method for a before/after claim

One run is not a measurement. Run-to-run spread on a development laptop is
several percent, wide enough to manufacture a win. Every comparison recorded
here is **three full runs per side, median taken per workload**, with both
sides built from the same profile on the same machine in the same sitting.

Note the release profile is `opt-level = "z"`: these are the numbers of the
size-optimized build the product actually ships, not of a speed-tuned build.

## Baseline and first optimization, 2026-08-22 / 23

Release profile, Apple Silicon development machine, 50 entries per run, three
runs per side.

| Workload | ns/instr before | ns/instr after | Change |
| --- | ---: | ---: | ---: |
| `i32_loop` | 8.548 | 6.456 | −24.5% |
| `i64_loop` | 7.604 | 5.343 | −29.7% |
| `f64_math` | 8.333 | 7.775 | −6.7% |
| `memory_scan` | 8.533 | 7.144 | −16.3% |
| `call_direct` | 18.161 | 17.197 | −5.3% |
| `call_indirect` | 19.147 | 17.871 | −6.7% |
| `br_table` | 8.443 | 7.382 | −12.6% |
| `local_shuffle` | 6.362 | 5.792 | −9.0% |

What the profiler said, and what changed because of it:

1. **A fifth of the interpreter's time was in one line** — the `push_operand`
   call inside `local.get`. `push_operand` asked `Vec::try_reserve(1)` on
   *every* push, so the hottest instruction in the engine carried an
   out-of-line allocator call. `Vec::push` allocates exactly when
   `len == capacity`, so the reserve now runs only in that case. Growth stays
   fallible — below capacity the push cannot allocate, at capacity the
   fallible reserve still runs first — and the static core grew by **0 bytes**.

2. **Arithmetic paid three `Vec` edge checks to shrink the stack by one.**
   Every `bin_*` / `un_*` helper popped both operands and pushed the result.
   These shapes never grow the stack, so they now read the operands in place
   and fold the result over the first one: one bounds check, no push, no
   `Option` unwrap.

The second change is also where the size gate earned its keep. The obvious
version — inlining the stack handling into each helper — ran fast and pushed
the static core from 101,240 to **117,752 bytes**, 15 KiB over the 100 KiB
product limit, because every helper is generic over its closure and is
monomorphised at each of its ~150 call sites. Routing the operand access
through non-generic shared functions (`top2_i32`, `fold_top2`, `set_top`, …)
kept each monomorphisation down to a couple of calls: same speed, **+16 bytes**
of core. Portability is ranked above throughput in the PRD, so the size gate
decided the shape of the optimization rather than being renegotiated around it.

Call-heavy rows barely moved on that pass, which was the honest read: their
cost is activation setup, not operand traffic.

## Second optimization: the argument buffer is the local frame

Profiling the two call rows on their own — `TINYVM_THROUGHPUT_ONLY` exists for
exactly this — put about 27% of their time in the allocator and in `memmove`.
Each guest call allocated an owned argument buffer, then allocated a second
buffer for the local frame and copied the arguments across.

The argument buffer *is* the head of the local frame, so it now becomes the
frame instead of being copied into one. A callee that declares no locals — the
common leaf — reaches its first instruction without allocating at all.

| Workload | ns/instr before | ns/instr after | Change |
| --- | ---: | ---: | ---: |
| `call_direct` | 17.197 | 15.643 | −9.0% |
| `call_indirect` | 17.871 | 16.824 | −5.9% |

Static core unchanged at 101,256 bytes. Against the original baseline the two
call rows are down 13.9% and 12.1%.

## Where the eight rows stand

Cumulative, against the 2026-08-22 baseline, medians of three runs per side:

| Workload | baseline | now | Change |
| --- | ---: | ---: | ---: |
| `i32_loop` | 8.548 | 6.640 | −22.3% |
| `i64_loop` | 7.604 | 5.631 | −25.9% |
| `f64_math` | 8.333 | 7.739 | −7.1% |
| `memory_scan` | 8.533 | 7.134 | −16.4% |
| `call_direct` | 18.161 | 15.643 | −13.9% |
| `call_indirect` | 19.147 | 16.824 | −12.1% |
| `br_table` | 8.443 | 7.573 | −10.3% |
| `local_shuffle` | 6.362 | 5.724 | −10.0% |

Static core 101,240 → 101,256 bytes, still under the 100 KiB gate.

## A win that was measured and then declined: f64

The same in-place transformation applies to the f64 arms, which are still
written as pop / pop / push. It was implemented and measured: `f64_math` goes
from 7.775 to 5.621 ns per instruction, **−27.7%**, the same size of win i64
got.

It was reverted, because it costs about 200 bytes of `__text` and the size gate
has no room for them. The linked core sits at 101,256 bytes against a 102,400
limit — 1.1 KiB of headroom — while the linked file grows in 16 KiB steps as
`__TEXT` crosses a page. A 192-byte text growth therefore measured as a
16,512-byte file growth and put the core 15 KiB over the product limit.

Three shapes were tried and all landed at the same place: helper functions
generic over a closure (+216 bytes), the same helpers with the libm calls
wrapped so no function item is taken by value (+216), and no new functions at
all with each arm calling the shared accessors directly (+192).

Spending the last kilobyte of a <100 KiB core on f64 throughput is the wrong
trade for this product: the TinyArcade embedding is deliberately i32-only, so
f64 is not on the critical path, while the size limit is the reason the engine
runs where JIT is forbidden. Whoever buys headroom back — by cutting
monomorphisation elsewhere, for instance — should take this change first: it is
a known, measured 27.7%.

## What is left

The remaining call cost is the two buffers still allocated per activation (the
operand stack and the control stack) and the activation bookkeeping itself.
Recycling those across activations is the obvious next step and is deliberately
*not* taken here: pooled buffers retain capacity that the host's
`max_activation_slots` ceiling does not account for, so it needs a bounded-pool
design rather than a quick free list. Boundedness outranks throughput.

Beyond that sit the load-time lowering and stack-top caching sketched in
[the performance notes](../prd/notes-performance.md).

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
