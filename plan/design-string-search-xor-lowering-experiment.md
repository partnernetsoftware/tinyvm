# String search XOR lowering: decisive experiment

This experiment may change the qjswasm emitter but adds no language feature,
public API, String metadata or tinyvm instruction. It decides whether the
already-supported Wasm `i32.xor` should replace a historical arithmetic
emulation in the four-byte String-search fast path.

| Field | Value |
|---|---|
| Date | 2026-09-04 |
| Purpose | Cut the dominant compare/branch slope without weakening semantics or the `<10` absolute search gate |
| Owner result | `research/string-search-cost-attribution/RESULTS.md` |
| Implementation | `crates/tinyvm-qjs/src/method.rs` |
| Evidence | `research/string-search-xor-lowering/RESULTS.md` |

## 0. Settled facts

```text
absolute miss cost = 10.5000 steps/byte
├─ loop                 2.7500
├─ i32.load             1.2500
├─ compare + branch     6.5000  <- only owner opened here
└─ exact verify + miss  0.0000
```

`skip_clear_window` predates qjswasm's `I32Xor` lowering. It still spells
`w ^ pattern` as `(w | pattern) - (w & pattern)`: eight guest instructions
including stack loads and local publication. Direct `i32.xor` needs four.
tinyvm already validates and executes this MVP instruction, so the proposed
change does not widen the VM.

## 1. Hard constraints

- Change only the XOR spelling inside `skip_clear_window`.
- Keep the has-zero-byte identity, four-byte window, tail behavior, valid UTF-8
  semantics, operation counter and String record unchanged.
- Use build-only subtraction at the same four lengths and toolchain as the
  owner attribution court. Historical `.length` subtraction is explanatory
  only.
- Run existing hit/miss, Unicode, window-boundary and full tinyvm-qjs tests.
- Record stripped/module byte impact beside operation cost; neither can be
  hidden by quoting only the favorable axis.
- Do not change `<10`, add source-pattern specialization, SIMD, metadata, a
  host callback or a new instruction.
- **Disease detector:** any desire to improve another part of the loop while
  this comparison is open is a new experiment, not cleanup for this one.

## 2. Minimal variants

| Variant | XOR spelling | Why included |
|---|---|---|
| A baseline | `(w | p) - (w & p)` | exact checked-in production truth |
| B direct | `w ^ p` through existing `Ins::I32Xor` | single-variable candidate |

Rejected from this court: scalar byte comparisons, wider windows, SIMD,
changing `~x`, String metadata and runtime-native search. They move another
axis and cannot explain whether the stale XOR spelling is worth removing.

## 3. Precommitted criteria

| ID | Kind | Criterion |
|---|---|---|
| X0 | Boolean | Existing String semantics and full `cargo test -p tinyvm-qjs` stay green |
| X1 | Slope | Direct variant absolute `includes` and `indexOf` miss slopes are both `<10.0` steps/byte |
| X2 | Slope | Direct variant improves both absolute slopes by at least 0.75 steps/byte versus A |
| X3 | Boundary | ASCII and valid UTF-8 have the same per-byte slope within 0.25 steps/byte; denominators remain separate |
| X4 | Size | Direct emitted modules are no larger than A for the same source; exact byte deltas are recorded |
| X5 | Reproducibility | Exact clean SHA, compiler, commands, raw totals and source hashes are recorded |

Slope gates X1-X3 outrank size. Size cannot rescue a runtime regression, and a
runtime win cannot hide unexpected module growth.

## 4. Decision tree, kill and time box

```text
X0 semantics green?
├─ no  -> REJECT direct XOR and revert
└─ yes -> X1 absolute gate + X2 improvement + X3 boundary pass?
          ├─ no  -> REJECT; keep arithmetic spelling
          └─ yes -> X4 non-growth pass?
                    ├─ no  -> REJECT; document the byte trade
                    └─ yes -> ACCEPT direct i32.xor
```

Kill immediately on any new VM instruction, String-layout change, semantic
special case or threshold edit. The time box ends once X0-X4 have numbers; do
not continue into wider-window or SIMD work.

## 5. Evidence layout

```text
research/string-search-xor-lowering/
├─ README.md
├─ measure.sh
└─ RESULTS.md
```

## 6. What this does not answer

- Whether 9.x steps/byte is the final desirable search cost.
- Whether wider windows, SIMD or a different algorithm win on other needles.
- Whether String metadata should be reopened.
- Whether tinyvm replaces Wasmtime outside the workload ladder in the
  qjswasm PRD.

## 7. Result

**ACCEPT direct `i32.xor`.** On exact measurement SHA `17db24d`, both
`includes` and `indexOf`, on ASCII and valid UTF-8, moved from 10.5000 to
9.5000 steps/byte: a 1.0000 improvement with identical intercepts. The emitted
module shrank by 6 bytes. Thus X0 → X1/X2/X3 → X4 all pass; X5 is recorded in
`research/string-search-xor-lowering/RESULTS.md`.

The result removes a stale lowering left from before `I32Xor` existed. It does
not claim that 9.5 is a terminal optimum or open any other search axis.
