# String search cost attribution: decisive experiment

This diagnostic does not enter must-ship scope, change product capability
state, change the String record, or change the production engine.

| Field | Value |
|---|---|
| Date | 2026-09-04 |
| Purpose | Separate search cost from the old O(n) `.length` subtraction, then locate the real per-character owner before opening another String optimization |
| Baseline | production `tinyvm` at `69a3e3b`; compact four-byte String record |
| Implementation | existing cost fixtures plus `#[cfg(test)]` cumulative probes in `crates/tinyvm-qjs/src/method.rs`; production compiler path is unchanged |
| Pre-reading | `plan/design-direct-string-metadata-publication-experiment.md`, `plan/design-index-of-window-skip.md` |
| Source discipline | Same interpreter, fixture subtraction and operation counter for every row; no task- or source-pattern specialization |

## 0. Question and settled facts

The direct-producer metadata experiment was correctly rejected: its inherited
search courts measured 10.5 steps per character against a frozen strict gate
`<10`. Post-verdict audit found that 7.2 -> 10.5 was not a slower search loop.
The court subtracted `return s.length` from the search call. Production
`.length` walks UTF-8 at about 3.3 steps per character, while the rejected
metadata candidate made it O(1); the subtraction therefore hid 3.3 steps per
character only in the old baseline. The search body retained the same
four-byte-window loop (apart from its body offset).

The next question is not whether 10.5 is “close enough”. It is first whether a
build-only control reproduces the search body's absolute slope, then which
layer owns that slope: fixed dispatch, loop control, byte read, comparison, or
miss return.

Already settled:

1. The `<10` gate remains unchanged; 10.5 is a failure.
2. The compact String record remains production truth.
3. General construction-time scans, side tables and lazy mutable metadata are
   rejected axes and do not reopen here.
4. This experiment produces an attribution table and the next experiment's
   owner. It does not produce an optimized engine.
5. The earlier D verdict remains REJECT: its precommitted court failed. Finding
   a weakness in that court after the verdict is evidence for the next ruler,
   not permission to rewrite the historical result.

## 1. Hard constraints

- Use exact interpreter operation counts, not wall-clock time, as the primary
  measure.
- Use the same release/debug posture, compiler, flags, fixtures and subtraction
  method for every probe; record them in `research/string-search-cost-attribution/RESULTS.md`.
- Measure at least four haystack lengths, including one short point and two
  points large enough for slope fitting. Needles and character distribution
  stay fixed within one series.
- Report raw totals, matched control totals, incremental totals, fitted slope
  and intercept. A single point is invalid evidence.
- Measure both controls at every length: build-only `return 0` and historical
  `return s.length`. The former owns absolute search cost; the latter exists
  only to reproduce and explain the old 7.2 result.
- Each probe adds exactly one layer to the preceding probe. The difference
  between adjacent probes is the attributed cost; independently written
  “equivalent” loops are not comparable.
- Preserve `includes` / `indexOf` semantics, UTF-16 indexing, valid UTF-8 and
  the existing step counter. Diagnostic instrumentation may not remove a
  check that production needs.
- No String-layout, ABI, allocator, instruction-set, budget or public API
  change is allowed.
- The sum of attributed slopes must reproduce the build-only-subtracted
  full-search slope within
  0.25 steps/character or 5%, whichever is larger. Otherwise the experiment
  is inconclusive.
- **Disease detector:** any urge to change the record, weaken `<10`, add a
  source-pattern shortcut, or turn a diagnostic helper into a production
  primitive is a finding to record—not permission to do it.

## 2. Minimal experiment

One generated diagnostic method body is assembled cumulatively and measured
at the same haystack lengths. This fixes the control-flow skeleton and changes
only the named layer.

| Probe | Added work | Why this isolates the layer |
|---|---|---|
| P0a build control | Construct the identical haystack and `return 0` | Leaves no O(n) work in the subtraction |
| P0b historical control | Construct it and `return s.length` | Reproduces how the old court hid the length-walk slope |
| P0 dispatch | Fixed search method dispatch; no character loop | Fits the intercept independently of length |
| P1 loop | Production outer bound plus four-byte position advance, but no body read | Adds loop-control slope at the same window width |
| P2 read | P1 plus the production `i32.load` into the existing scratch local | Adds the four-byte read slope without comparison |
| P3 compare | P2 plus the production has-zero-byte calculation and clear-window branch | Adds the comparison/branch slope on the primary absent-byte court |
| P4 exact + miss | P3 plus the production tail/exact verifier and false result | Adds exact verification and miss completion; expected to have zero primary slope when every full window is clear |
| P5 full | Existing `includes` and `indexOf` miss fixtures | Cross-checks that the decomposition represents the real methods |

Series:

- ASCII haystack with an absent one-code-unit needle: primary slope court.
- Non-ASCII valid UTF-8 haystack with an absent one-code-unit needle:
  attribution boundary check, reported separately and never averaged into the
  ASCII result.
- Existing four-byte skip-window fixture: independent historical calibration.
  Its current length-subtracted result must reproduce within the fixture's
  existing exact tolerance. The new build-only subtraction must report the
  absolute slope beside it; the difference between the two must equal the
  independently measured `.length` slope within the closure tolerance.

No new Wasm instruction, profiler, String representation or public benchmark
command is part of the minimum.

## 3. Precommitted criteria

| ID | Property | Kind | Frozen criterion |
|---|---|---|---|
| C0 | Semantic/control validity | Boolean | Existing search semantic tests and full `cargo test -p tinyvm-qjs` remain green |
| C1 | Calibration | Boolean | Existing length-subtracted search fixture reproduces its checked-in reference under the recorded toolchain |
| C2 | Linearity | Slope | Each P1-P5 ASCII series has fit residual at most 0.25 steps/character and no point differs from its fit by more than 2% |
| C3 | Ruler closure | Safety | `absolute search slope - historical slope` equals the independently measured `.length` slope within max(0.25 steps/character, 5%) |
| C4 | Attribution closure | Safety | Sum of P1-P4 adjacent slope deltas is within max(0.25 steps/character, 5%) of P5's absolute slope |
| C5 | Actionable owner | Slope | One layer owns at least 0.50 steps/character; only that layer may receive the next frozen optimization experiment |
| C6 | Fixed overhead | Intercept | P0-dispatch and P4 report dispatch and miss-return intercepts separately; neither may be described as per-character cost |
| C7 | Unicode boundary | Checklist | Non-ASCII results name byte/code-unit/code-point denominators separately; no cross-denominator ratio is published |
| C8 | Reproducibility | Boolean | `RESULTS.md` includes exact SHA, compiler identity, commands, raw totals, subtraction inputs and an independent reference hash |

Priority is C0-C4 validity, then the C5 slope decision, then C6-C8 explanatory
evidence. Intercepts cannot overrule a slope result.

## 4. Decision tree, kill criteria and time box

```text
C0 semantics green and C1 historical calibration reproduced?
├─ no  -> INVALID; repair the measuring court, publish no attribution
└─ yes -> C2 linearity, C3 ruler closure and C4 attribution closure pass?
          ├─ no  -> INCONCLUSIVE; keep production pin and redesign probes
          └─ yes -> C5 identifies one actionable owner?
                    ├─ yes -> freeze one orthogonal experiment for that owner
                    └─ no  -> STOP; cost is distributed, keep current engine
```

- A dominant owner in loop control opens a loop/branch-dispatch experiment.
- A dominant owner in the four-byte read opens a bounded load experiment.
- A dominant owner in the has-zero comparison/branch opens an instruction or
  branch-lowering experiment.
- A slope attributed to exact verification + miss means the absent-byte court
  is reaching work it should avoid; verdict is inconclusive.
- Fixed dispatch may justify a separate intercept experiment, but cannot be
  offered as the answer to the 0.5 steps/character question.

Kill immediately if a probe needs a new runtime primitive, changes String
layout, changes the operation counter, weakens an existing court, or cannot
retain the production loop's checks.

The time box ends when C0-C5 have numbers and the tree reaches an exit. Do not
implement the selected optimization inside this experiment.

Every criterion is accounted for: C0-C1 open the court; C2-C4 validate the
measurement; C5 selects or rejects a next structural experiment; C6-C8 are
mandatory result fields but cannot change the branch.

## 5. Evidence layout

```text
research/string-search-cost-attribution/
├── README.md          # probe construction and fixture map
├── measure.sh         # bounded reproducible runner
├── raw/               # ignored raw measurements
└── RESULTS.md         # C0-C8 table and decision trace
```

Existing semantic and cost fixtures remain the source of truth. Temporary
generated files and raw outputs stay ignored; the commands and aggregate
numbers are checked into `RESULTS.md`.

## 6. Excluded alternatives

| Alternative | Why excluded |
|---|---|
| Round 10.5 down or change `<10` to `≤11` | Post-result goalpost change |
| Restore direct metadata and optimize while measuring | Mixes representation, producer and search axes |
| Wall-clock benchmark | Scheduler noise hides interpreter operation ownership |
| Separate hand-written loops for each layer | Changes control flow along with the measured layer |
| Add a profiler instruction or host callback | Measures instrumentation overhead and expands the runtime |
| Optimize ASCII only and infer Unicode | Confuses byte, code-unit and code-point denominators |

## 7. This experiment does not answer

- Whether direct String metadata should be reconsidered after some future
  orthogonal optimization.
- Whether ropes, interning, GC, SIMD, JIT, new Wasm instructions or type-flow
  specialization are worthwhile.
- Whether qjswasm can replace Wasmtime workload by workload; that larger ladder
  is owned by the qjswasm PRD.
- Which optimization wins. This experiment only identifies the layer that may
  receive a separately frozen experiment.

## 8. Results and verdict

**Decided: the has-zero-byte comparison and clear-window branch own the next
experiment.** At exact court SHA `e7c9097`, the absolute miss slope decomposes
as 2.7500 loop + 1.2500 load + **6.5000 compare/branch** + 0.0000 exact/miss =
10.5000 steps per byte. All four-point fits have zero measured residual. The
same per-byte slopes repeat on valid UTF-8; code-point and UTF-16-unit views are
reported separately rather than compared across denominators.

Decision trace: C0/C1 pass → C2/C3/C4 pass → C5 selects compare/branch because
6.5000 exceeds the frozen 0.50 threshold → freeze one orthogonal
instruction/branch-lowering experiment. C6 records a 76-step fixed dispatch
intercept and no additional P4 slope; C7/C8 pass. The historical direct
metadata verdict remains REJECT, and production code, String layout, operation
counter and `<10` gate are unchanged.

Full raw totals, exact identity, commands, source hashes, denominator
translations and deviations are in
`research/string-search-cost-attribution/RESULTS.md`.
