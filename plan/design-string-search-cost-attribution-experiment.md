# String search cost attribution: decisive experiment

This diagnostic does not enter must-ship scope, change product capability
state, change the String record, or change the production engine.

| Field | Value |
|---|---|
| Date | 2026-09-04 |
| Purpose | Locate the remaining per-character cost in `includes` / `indexOf` before opening another String optimization |
| Baseline | production `tinyvm` at `69a3e3b`; compact four-byte String record |
| Implementation | existing `crates/tinyvm-qjs` cost fixtures plus temporary diagnostic probes; no accepted source change |
| Pre-reading | `plan/design-direct-string-metadata-publication-experiment.md`, `plan/design-index-of-window-skip.md` |
| Source discipline | Same interpreter, fixture subtraction and operation counter for every row; no task- or source-pattern specialization |

## 0. Question and settled facts

The direct-producer metadata experiment was correctly rejected: its search
courts measured 10.5 steps per character against a frozen strict gate `<10`.
The next question is not whether 10.5 is “close enough”. It is which layer
owns that slope: fixed dispatch, loop control, code-unit read, comparison, or
miss return.

Already settled:

1. The `<10` gate remains unchanged; 10.5 is a failure.
2. The compact String record remains production truth.
3. General construction-time scans, side tables and lazy mutable metadata are
   rejected axes and do not reopen here.
4. This experiment produces an attribution table and the next experiment's
   owner. It does not produce an optimized engine.

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
- Each probe adds exactly one layer to the preceding probe. The difference
  between adjacent probes is the attributed cost; independently written
  “equivalent” loops are not comparable.
- Preserve `includes` / `indexOf` semantics, UTF-16 indexing, valid UTF-8 and
  the existing step counter. Diagnostic instrumentation may not remove a
  check that production needs.
- No String-layout, ABI, allocator, instruction-set, budget or public API
  change is allowed.
- The sum of attributed slopes must reproduce the full-search slope within
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
| P0 control | Call/return and fixed method dispatch; no character loop | Fits the intercept independently of length |
| P1 loop | The production loop bounds and index update, but no body read | Adds loop-control slope |
| P2 read | P1 plus the production code-unit/byte load path; value is consumed | Adds read/decode slope without comparison |
| P3 compare | P2 plus the same equality/prefix test, forced to miss | Adds comparison slope |
| P4 miss | P3 plus production miss completion and result formation | Adds miss-return cost; expected to affect intercept, not slope |
| P5 full | Existing `includes` and `indexOf` miss fixtures | Cross-checks that the decomposition represents the real methods |

Series:

- ASCII haystack with an absent one-code-unit needle: primary slope court.
- Non-ASCII valid UTF-8 haystack with an absent one-code-unit needle:
  attribution boundary check, reported separately and never averaged into the
  ASCII result.
- Existing four-byte skip-window fixture: independent calibration. Its current
  result must reproduce within the fixture's existing exact tolerance before
  any new number is accepted.

No new Wasm instruction, profiler, String representation or public benchmark
command is part of the minimum.

## 3. Precommitted criteria

| ID | Property | Kind | Frozen criterion |
|---|---|---|---|
| C0 | Semantic/control validity | Boolean | Existing search semantic tests and full `cargo test -p tinyvm-qjs` remain green |
| C1 | Calibration | Boolean | Existing search cost fixture reproduces its checked-in reference under the recorded toolchain |
| C2 | Linearity | Slope | Each P1-P5 ASCII series has fit residual at most 0.25 steps/character and no point differs from its fit by more than 2% |
| C3 | Attribution closure | Safety | Sum of P1-P4 adjacent slope deltas is within max(0.25 steps/character, 5%) of P5's slope |
| C4 | Dominant owner | Slope | One layer owns at least 0.50 steps/character and at least 40% of the excess above the inherited `<10` gate |
| C5 | Fixed overhead | Intercept | P0 and P4 report dispatch and miss-return intercepts separately; neither may be described as per-character cost |
| C6 | Unicode boundary | Checklist | Non-ASCII results name byte/code-unit/code-point denominators separately; no cross-denominator ratio is published |
| C7 | Reproducibility | Boolean | `RESULTS.md` includes exact SHA, compiler identity, commands, raw totals, subtraction inputs and an independent reference hash |

Priority is C0-C3 validity, then the C4 slope decision, then C5-C7 explanatory
evidence. Intercepts cannot overrule a slope result.

## 4. Decision tree, kill criteria and time box

```text
C0 semantics green and C1 calibration reproduced?
├─ no  -> INVALID; repair the measuring court, publish no attribution
└─ yes -> C2 linearity and C3 attribution closure pass?
          ├─ no  -> INCONCLUSIVE; keep production pin and redesign probes
          └─ yes -> C4 identifies one dominant owner?
                    ├─ yes -> freeze one orthogonal experiment for that owner
                    └─ no  -> STOP; cost is distributed, keep current engine
```

- A dominant owner in loop control opens a loop/branch-dispatch experiment.
- A dominant owner in code-unit read opens a bounded read/decode experiment.
- A dominant owner in comparison opens a comparison/prefix experiment.
- A slope attributed to miss return means the decomposition is wrong, because
  miss completion is once per search; verdict is inconclusive.
- Fixed dispatch may justify a separate intercept experiment, but cannot be
  offered as the answer to the 0.5 steps/character question.

Kill immediately if a probe needs a new runtime primitive, changes String
layout, changes the operation counter, weakens an existing court, or cannot
retain the production loop's checks.

The time box ends when C0-C4 have numbers and the tree reaches an exit. Do not
implement the selected optimization inside this experiment.

Every criterion is accounted for: C0-C1 open the court; C2-C3 validate the
measurement; C4 selects or rejects a next structural experiment; C5-C7 are
mandatory result fields but cannot change the branch.

## 5. Evidence layout

```text
research/string-search-cost-attribution/
├── README.md          # probe construction and fixture map
├── measure.sh         # bounded reproducible runner
├── raw/               # ignored raw measurements
└── RESULTS.md         # C0-C7 table and decision trace
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

Not run. This section must be filled with the C0-C7 table, exact decision-tree
trace, deviations, an explicit statement that the measures were not changed to
improve the verdict, and every result that contradicted the initial hypothesis.
