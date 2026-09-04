# String search cost attribution results

Status: **partial — ruler closed; P0-dispatch through P4-miss not run**.

## Identity

| Field | Value |
|---|---|
| Engine/court SHA | `82b6491e06e5b64f85411ba73ef54b4233e4bdc7` |
| Court source SHA-256 | `31c67fa3b02c049d2611bdf6f82a7fe2488617c5c07f5d95a8fa951549597d22` |
| Rust | `rustc 1.97.0 (2d8144b78 2026-07-07)` |
| LLVM | `22.1.6` |
| Host | `aarch64-apple-darwin` |
| Command | `CARGO_TARGET_DIR=target/search-attribution ./research/string-search-cost-attribution/measure.sh` |

The source tree was clean at the recorded SHA. The test is an ignored research
court, so the ordinary suite compiles it without paying for the four large
series.

## Raw totals

| length | build | `.length` | `includes` | `indexOf` | length − build | includes − build | includes − length | indexOf − build | indexOf − length |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2,048 | 11,975 | 18,815 | 33,521 | 33,564 | 6,840 | 21,546 | 14,706 | 21,589 | 14,749 |
| 8,192 | 43,191 | 69,999 | 129,249 | 129,292 | 26,808 | 86,058 | 59,250 | 86,101 | 59,293 |
| 32,768 | 166,580 | 273,260 | 510,686 | 510,729 | 106,680 | 344,106 | 237,426 | 344,149 | 237,469 |
| 131,072 | 658,635 | 1,084,803 | 2,034,933 | 2,034,976 | 426,168 | 1,376,298 | 950,130 | 1,376,341 | 950,173 |

Least-squares slopes over all four lengths:

| Series | Steps / character |
|---|---:|
| independent `.length` cost | **3.2500** |
| `includes`, build-only subtraction | **10.5000** |
| `includes`, historical `.length` subtraction | **7.2500** |
| `indexOf`, build-only subtraction | **10.5000** |
| `indexOf`, historical `.length` subtraction | **7.2500** |

## Criteria trace

| Criterion | Result | Evidence |
|---|---|---|
| C0 semantics/control | **pass** | `cargo test -p tinyvm-qjs` green at the exact SHA; ignored wide/evidence courts remain explicitly ignored |
| C1 historical calibration | **pass** | existing `index_of_cost` gate green; four-point historical slope is 7.2500 |
| C2 probe linearity | **pending** | P1-P4 do not exist yet |
| C3 ruler closure | **pass** | `10.5000 − 7.2500 = 3.2500` independently for both methods |
| C4 attribution closure | **pending** | P1-P4 do not exist yet |
| C5 actionable owner | **pending** | no layer attribution yet |
| C6 fixed overhead | **pending** | P0-dispatch and P4-miss not measured |
| C7 Unicode boundary | **pending** | this first court is ASCII only |
| C8 reproducibility | **pass for this phase** | exact SHA, compiler, command, raw totals and independent source hash recorded |

## Partial verdict

The earlier direct-metadata verdict remains **REJECT** because its precommitted
court failed. The new evidence changes the interpretation of the numbers, not
that historical branch:

```text
old 7.2500 = absolute search 10.5000 − O(n) `.length` 3.2500
```

Therefore 7.2 → 10.5 was not a search-loop regression. The old subtraction is
not a valid cross-implementation ruler when `.length` itself changes from O(n)
to O(1). Future attribution and optimization courts must use the build-only
control. No engine change is accepted, and no P1-P4 owner is inferred from this
phase.
