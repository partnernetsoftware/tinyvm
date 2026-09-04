# Static `length` dispatch: decisive experiment

Status: **completed; Variant C rejected and implementation rolled back**

Date: 2026-09-04  
Baseline: `0a43271`; active engine source remains `028a914`  
Purpose: decide whether a static-property dispatcher can make eager String
metadata pass the frozen 160-step `.length` court without weakening dynamic
JavaScript property behavior  
Implementation location: `crates/tinyvm-qjs`; no public feature or ABI  
Pre-reading: `plan/design-string-record-metadata-experiment.md`

This is an upstream engine experiment. It does not change AgenTerm capability
status or its tinyvm pin unless every gate below selects the candidate.

## 0. Decision and settled facts

Should the compiler lower a statically spelled `.length` through a dedicated
length-property prefab, or keep sending it through the generic property-key
dispatcher?

Already settled:

1. The previous eager-metadata candidate flattened all relevant slopes and cut
   the 1,000-position traversal from 3,009,139 to 228,053 steps.
2. It was correctly rejected because repeated `.length` cost 166 steps/call,
   above the precommitted 160 limit. That verdict is not reopened.
3. String values remain dynamically typed. This experiment may specialize the
   known key `length`; it may not assume the receiver is a String.
4. The four-byte record remains production truth until this experiment wins.

## 1. Hard constraints

- Preserve JavaScript behavior for String, Array, ordinary Object, Number,
  `null` and `undefined` receivers.
- `{length: 7}.length`, `array.length`, `string.length` and missing `length`
  must remain distinct observable cases.
- Computed `value["length"]` remains on the generic property path in this
  experiment; it is a parity witness, not an optimization target.
- Reconstruct the prior gated eight-byte String record exactly; do not alter
  its host pointer/length ABI, allocation checks or one-layout-per-module seal.
- Do not raise steps, pages, source, output, deadline or recursion limits.
- Programs that cannot reach String metadata retain the four-byte record and
  compile byte-identically to the baseline.
- No task name, source-text pattern, AgenTerm vocabulary or benchmark-specific
  branch may affect lowering.
- **Disease detector:** any urge to add static receiver typing, an IR escape
  hatch, a second public value tag or another property primitive merely to make
  this court green is a finding, not permission to widen the experiment.

## 2. Minimal variants

| Variant | Representation | Static `.length` lowering | Why included |
|---|---|---|---|
| A | current four-byte record | generic property dispatcher | production control |
| B-reference | gated eight-byte metadata | generic property dispatcher | already measured 166-step near miss; reconstructed only as an intermediate check |
| C | same gated eight-byte metadata | one internal static-length prefab | isolates key dispatch from representation |

The C prefab receives the dynamic value pair and must implement the same
receiver dispatch as the existing path. It may skip `ToPropertyKey` and the
runtime String comparison with the pooled word `length`, because the AST has
already proved that exact static key. All other policy stays in existing
helpers. Only one final candidate is packed.

## 3. Precommitted criteria

Measure exact VM steps using the same fixtures and subtraction method as the
previous experiment. Record compiler/toolchain identity and commands beside
the result.

| ID | Property | Gate |
|---|---|---|
| C1 | Boolean semantics | static and computed `length` agree for String, Array, `{length:7}`, missing property and all existing error receivers |
| C2 | Boolean safety | host ABI, heap attacks, malformed pointers, typed exhaustion and full `tinyvm-qjs` suite green |
| C3 | Intercept | repeated `.length` at 64 and 6,000 ASCII is ≤160 steps/call |
| C4 | Slope | the two C3 points differ by ≤16 steps/call |
| C5 | Position | `charCodeAt(999)` and `s[999]` each ≤320 incremental steps; full 1,000 traversal ≤600,000 |
| C6 | Workload | `only_chars` gains ≥3× at 160 and 640 bytes; 640/160 per-byte ratio ≤1.35 |
| C7 | Non-user bytes | a program outside the representation gate is byte-identical to A |
| C8 | Opt-in bytes | growth from A ≤ `4 × string_record_count + 768` bytes; no second allocation |
| C9 | Memory | representative AgenTerm workflow peak guest pages grow ≤5% |
| C10 | Downstream | exact winning commit passes AgenTerm qjswasm crate tests and one release-critical `.qjs` journey without a budget change |

C3/C4 are the first execution gate. C5-C10 cannot rescue their failure.
Size uses whole emitted Wasm bytes for the same source/build; memory uses VM
peak pages for the same workflow. No cross-tool or cross-program ratio counts.

## 4. Decision tree, kill criterion and time box

```text
C1/C2 semantics or safety fails?
├─ yes → reject C, rollback all implementation
└─ no
   ├─ C3 or C4 misses? → reject C immediately; retain measurements only
   └─ pass
      ├─ C5 or C6 misses? → reject metadata+dispatcher; investigate loop form separately
      └─ pass
         ├─ C7/C8/C9 misses? → reject C; compact representation wins
         └─ pass
            ├─ C10 misses? → reject C; upstream microbench value did not transfer
            └─ pass → accept C, remove experiment switch, then bump AgenTerm pin
```

Kill immediately for a hard-constraint violation, two coexisting record layouts
inside one module, a second allocation, or a task-specific shortcut. The first
time box ends as soon as C1-C4 have exact results. Only a pass opens C5-C10.

Every C1-C10 item appears in the tree: C1/C2 are safety first, C3/C4 are the
decisive constant/slope pair, C5/C6 test transferred work, C7-C9 charge size
and memory, and C10 is downstream transfer. Every branch has one verdict.

## 5. Evidence layout

- This file owns the specification and final verdict.
- Existing `length_cost`, `char_access_cost`, representation, heap and host-door
  tests own executable evidence; add focused fixtures rather than a parallel
  benchmark harness.
- Temporary measurement output stays untracked. Only reproducible commands and
  final numbers enter this file.

## 6. Excluded alternatives

| Alternative | Reason excluded |
|---|---|
| Raise 160 to 166 | post-measurement goalpost move |
| Hoist one loop's `.length` | does not fix arbitrary repeated dynamic reads |
| Infer the receiver's static type | new compiler architecture, not key dispatch |
| Optimize computed `value["length"]` too | mixes key conversion with this isolated axis |
| Add a host helper for string loops | moves portable logic across the authority boundary |
| Rope, GC, interning or lazy side table | unrelated representation/lifetime decisions |

## 7. Not answered

- Whether a future type-flow pass is worthwhile.
- Whether general static properties besides `length` deserve direct prefabs.
- Whether loop/iterator lowering beats repeated indexed access.
- Whether richer Unicode normalization belongs in the language subset.

## 8. Results and verdict

Variant C was reconstructed from the prior Variant B evidence, with the same
gated eight-byte record and one internal `__static_length` prefab. The prefab
received the dynamic value pair, answered String from the cached metadata, and
delegated every non-String receiver to the pre-existing Object/Array property
helper. No public tag, host helper, receiver typing, second allocation or
budget change was introduced.

Measurement environment: pinned repository `rust-toolchain.toml`, `rustc
1.97.0 (2d8144b78 2026-07-07)`, Darwin arm64 host. VM steps are exact
interpreter counts; all executions used the existing default limits unless a
pre-existing test supplied its own stricter or larger bound.

| Gate | Result | Evidence |
|---|---|---|
| C1 semantics | **pass** | Static and computed `length` agreed for String, Array, `{length: 7}`, missing Object property, Number, `null` and `undefined`; existing UTF-16 and producer cases passed. |
| C2 safety/full suite | **fail** | Host/heap/unwind/JSON and focused representation semantics passed, but `cargo test -p tinyvm-qjs --no-fail-fast` ended with 21 failing targets. Several were stale four-byte test readers, but existing performance courts also failed materially. |
| C3 intercept | pass, non-promoting | 64 ASCII = **160 steps/call**; 6,000 ASCII = **160 steps/call**. |
| C4 slope | pass, non-promoting | Absolute difference = **0 steps/call**. |
| C5-C10 | not reached | C2 failure closes the decision tree before transferred-work, size, memory and downstream gates. |

Representative C2 regressions that cannot honestly be classified as test-side
record readers:

- Array `join` over ten-byte Strings measured **573.9 steps/element**, missing
  the existing `<260` court.
- `JSON.stringify` of a 1,000-byte plain String measured **73 steps/byte**,
  missing the existing `<50` court.
- A journal-record stringify measured **68,490 steps**, missing `<60,000`.
- A 30-property flat Object measured **1,536 steps/property**, missing
  `<1,500`.
- The long `includes`/`indexOf` and `split` courts also crossed their existing
  limits (10.5 and 63.5 steps/character respectively). These are baseline
  subtraction effects and construction scans introduced by the opted-in
  representation, not evidence that the old limits should be raised.

The C3/C4 probe was run after C1 and while the complete C2 inventory was being
classified. That ordering deviated from §4's strict stop-at-C2 rule, but did
not alter the verdict: the 160/160 result is recorded as diagnostic only and
was not allowed to rescue C2. No criterion or existing performance limit was
changed after seeing the measurements.

Reproduction commands used before rollback:

```sh
cargo test -p tinyvm-qjs --test string_length_m3 -- --nocapture
cargo test -p tinyvm-qjs --test length_cost \
  static_length_dispatch_meets_the_frozen_intercept_and_slope -- --nocapture
cargo test -p tinyvm-qjs --no-fail-fast
```

### Verdict trace

`C1 pass → C2 fail → reject C`; C3/C4 happened to pass at the exact intercept
ceiling but are not a later override branch. **Variant C is rejected.** The
production four-byte record remains the winner. All engine and test changes
were rolled back; this section is the only retained experiment artifact.

The useful finding is narrower than “metadata is bad”: a static-key prefab can
remove the last six steps and flatten `.length`, but eager publication through
general post-construction scans makes existing multi-string workloads regress.
A future experiment would have to pre-register direct per-producer metadata
publication and its own compatibility courts; it may not reopen this verdict
or silently relax the existing cost gates.
