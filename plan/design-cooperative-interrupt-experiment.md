# Cooperative interpreter interruption experiment

Date: 2026-09-09

Purpose: decide the smallest generic interruption seam for a running wasm call

Implementation: `research/cooperative-interrupt/` and `crates/tinyvm/src/wasm.rs`

Prerequisites: `prd/PRD.md`, `docs/tinyvm-interpreter-throughput.md`

Source discipline: repository-owned implementation and reproducible local measurements

This is a tinyvm robustness capability required by downstream embedders. It does not add
an agent permission boundary, wall-clock policy, host vocabulary, or private opcode.

## 0. Settled facts

1. `Limits::max_steps` bounds pure computation but cannot promptly honor an external cancel.
2. Host callbacks can already stop cooperatively, but a guest loop need not call a host.
3. The supervisor remains the hard process-containment deadline.
4. Interruption is a top-level invocation fact, not immutable module metadata.

## 1. Hard constraints

- `Module` remains a reusable program definition; one invocation's cancel identity must not persist
  into the next invocation.
- `Limits` remains `Clone + Copy` and contains only numeric resource ceilings.
- The core remains `no_std`, safe Rust, standard wasm, and unaware of wall clocks or threads.
- Existing `eval`, `invoke`, start-function, replay, and C ABI behavior remains unchanged unless a
  caller explicitly chooses the new seam.
- An interrupted call returns one distinct typed core fault, never `step budget` and never success.
- Polling adds no allocation and no host call in the instruction loop.
- Disease detector: any temptation to store an embedder-owned pointer in `Limits`, add a private
  opcode, or teach the core wall-clock policy is a finding, not an allowed shortcut.

## 2. Minimal variants

| Variant | Shape | Why included |
|---|---|---|
| A | Store `Arc<AtomicBool>` in `Module`/`Instance` | Lowest apparent wiring cost; tests whether persistence contamination is real |
| B | Borrow `&AtomicBool` only for one top-level invocation | Preserves module purity and gives the current call an exact cancel identity |
| C | Borrow a callback or store an epoch counter | Fallback only if B cannot meet portability, latency, or throughput gates |

Only B is implemented first. A is structurally rejected if a persistent instance cannot run a
second call with a fresh false flag after interrupting the first. C remains excluded unless a gate
below kills B.

## 3. Precommitted criteria

| ID | Property | Gate |
|---|---|---|
| C1 | Boolean / ownership | one persistent Instance accepts a different interrupt identity per call; no flag is stored in `Module`, `Limits`, or serialized state |
| C2 | Boolean / result | a pure infinite loop interrupted after 100 ms returns `interrupted`, not `step budget`, within 150 ms of the request |
| C3 | Boolean / control | the same loop with no request still reaches `step budget`; a short call with a false flag remains successful |
| C4 | Safety / portability | default and `no_std` builds pass; public C configuration layout is unchanged |
| C5 | Regression | existing replay and qjswasm host-operation cancellation results remain unchanged |
| C6 | Throughput | the existing `i32_loop` measurement regresses by no more than 5% against the same-command baseline |
| C7 | Size | the existing static-core `<100 KiB` gate remains green |

Measurements use the repository's existing commands and artifact boundaries; no new byte-size
number may be compared across a different build profile or tool.

## 4. Decision tree, kill criterion, and timebox

1. If C1, C3, C4, or C5 fails, reject B immediately; do not patch around the failed ownership seam.
2. If C2 fails, reduce the polling interval once. If it still fails, reject B and test C.
3. If C6 alone fails, raise the polling interval from 1024 to 65536 instructions and rerun C2/C6.
4. If C7 fails, inspect emitted size once; if the seam cannot stay under the existing gate, reject B.
5. Otherwise accept B.

Kill criterion: any need for stored raw pointers, module-persistent cancellation identity, a private
opcode, or wall-clock/thread policy inside tinyvm rejects the variant immediately.

Timebox: stop once C1-C7 have one reproducible result. Do not add iOS UI integration, a timer thread,
or unrelated interpreter optimization in this experiment.

## 5. Result layout

`research/cooperative-interrupt/RESULTS.md` records commands, baseline/candidate measurements,
criterion outcomes, decision trace, deviations, and the final accepted or rejected seam.

## 6. Excluded choices

| Choice | Reason |
|---|---|
| Interrupt field in `Limits` | destroys `Copy` or requires a raw pointer and mixes policy with numeric ceilings |
| Module-owned `Arc<AtomicBool>` | makes invocation identity persistent and invites stale cancellation across calls |
| Callback in the hot loop | lifetime/type erasure spreads through the core and adds an indirect call at each poll |
| Internal wall-clock thread | violates `no_std` and duplicates embedder/supervisor policy |
| Private wasm opcode | breaks the standard-wasm product boundary |

## 7. Not answered

- wall-clock deadline selection in any embedder;
- cancellation of a native host callback that is itself blocked;
- iOS lifecycle/UI wiring;
- forced termination after cooperative grace expires.

## 8. Result

Accepted: variant B, a call-scoped borrowed `&AtomicBool`, passed C1-C7. The core polls every
1024 guest instructions and returns the distinct `Interruption` fault class. The flag is carried
only in ephemeral call contexts; it is not stored in `Module`, `Instance`, `Limits`, replay state,
or the C ABI. The persistent-instance control proved that a later call can use a fresh flag.

The 100 ms asynchronous request returned in 0.11 s total, the same loop without a request still
reached `step budget`, default and `no_std` tests passed, Clippy passed with warnings denied, and
the static core stayed at 101,256 bytes with self-test result 42. The three-run `i32_loop` median
moved from 7.783 to 7.535 ns/instruction (-3.2%); no measured workload regressed by 5%.

The detailed evidence and deviations are in `research/cooperative-interrupt/RESULTS.md`.
