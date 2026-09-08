# Cooperative interpreter interruption results

Date: 2026-09-09

Decision owner: `plan/design-cooperative-interrupt-experiment.md`

Candidate: call-scoped borrowed `&AtomicBool`, polled every 1024 guest instructions

## Verdict

Accept variant B. It meets all precommitted criteria without storing an invocation identity in
`Module`, `Instance`, `Limits`, serialized state, or the C ABI. Tinyvm supplies a generic typed
interruption seam; embedders continue to own deadlines, timer threads, and forced containment.

## Functional and portability evidence

From repository root, with the dedicated `target/cooperative-interrupt/` lane:

```text
cargo test -p tinyvm --test cooperative_interrupt --test error_classification
  2 cooperative interruption tests passed
  6 error-classification tests passed

cargo test -p tinyvm --lib --no-default-features
  116 passed

cargo clippy -p tinyvm --all-targets -- -D warnings
  passed

crates/tinyvm/measure-core.sh
  static core: 101256 bytes; selftest rc=42
  OK: < 100 KiB and selftest==42
```

The asynchronous test starts an infinite standard-wasm loop, sets the borrowed flag after
100 ms, and requires a typed interruption within the following 150 ms. It completed in 0.11 s.
The persistent-instance test also proves that a fresh false flag does not inherit an earlier
interruption, and that the old entry point without a flag still terminates by `step budget`.

## Throughput evidence

The existing release throughput gate ran three times per side on the same development host in
one sitting with 50 entries per workload. Baseline bytes came from a clean archive of commit
`58ec897`; candidate bytes contained only this experiment's interpreter change. Values are the
median nanoseconds per guest instruction.

| Workload | Baseline | Candidate | Change |
|---|---:|---:|---:|
| `i32_loop` | 7.783 | 7.535 | -3.2% |
| `i64_loop` | 6.830 | 6.720 | -1.6% |
| `f64_math` | 8.892 | 8.614 | -3.1% |
| `memory_scan` | 8.712 | 8.271 | -5.1% |
| `call_direct` | 16.527 | 16.942 | +2.5% |
| `call_indirect` | 17.881 | 17.630 | -1.4% |
| `br_table` | 8.595 | 8.750 | +1.8% |
| `local_shuffle` | 6.855 | 6.668 | -2.7% |

The precommitted `i32_loop` limit was at most 5% regression; it passed. No row regressed by 5%.
Several apparent improvements are ordinary laptop noise and are not claimed as optimizations.

## Criterion trace

| Criterion | Result | Evidence |
|---|---|---|
| C1 call ownership | PASS | persistent Instance accepts a fresh flag; flag exists only in call contexts |
| C2 asynchronous result | PASS | 100 ms request returns `Interruption` in 0.11 s total |
| C3 control | PASS | false flag succeeds; legacy entry reaches `step budget` |
| C4 portability | PASS | default Clippy/tests and `no_std` library tests pass; C ABI unchanged |
| C5 regression | PASS | error taxonomy tests pass; qjswasm wiring remains a downstream increment |
| C6 throughput | PASS | three-run `i32_loop` median -3.2%; worst regression across rows +2.5% |
| C7 static size | PASS | 101,256 bytes; self-test 42 |

## Deviations and limits

- An early baseline attempt overlapped an unfinished shared-checkout edit and failed to compile.
  It is invalid and excluded. The recorded baseline used a clean archive of the exact prior commit.
- Clippy initially rejected three eight-argument internal interpreter functions. Moving the flag
  into their shared resource context made the static-core executable jump one 16 KiB Mach-O page
  to 117,768 bytes, so that shape was rejected by C7. The size-preserving call-stack shape uses a
  narrow `too_many_arguments` allowance on those three private functions; no public API or stored
  state is widened.
- Start-section execution still uses the legacy uninterruptible entry. This experiment deliberately
  covers explicit top-level invocations; adding an interruptible instantiate/start API requires its
  own ownership and partial-instantiation contract.
- Native host callbacks that block do not become interruptible through this seam. The embedding
  supervisor remains the hard containment boundary.
