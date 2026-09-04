# String search cost attribution results

Status: **decided — compare/branch owns the next experiment**.

## Identity

| Field | Value |
|---|---|
| Engine/court SHA | `e7c90976b3bb6147595b26eee6fe06ebf6f996cd` |
| Cumulative court source SHA-256 | `8a0611fc9ad0abd7ae2185d867b7ee8cc66d558063f119d3e08511e7d7d6f9f7` |
| Dual-control source SHA-256 | `31c67fa3b02c049d2611bdf6f82a7fe2488617c5c07f5d95a8fa951549597d22` |
| Rust | `rustc 1.97.0 (2d8144b78 2026-07-07)` |
| LLVM | `22.1.6` |
| Host | `aarch64-apple-darwin` |
| Command | `CARGO_TARGET_DIR=target/search-attribution ./research/string-search-cost-attribution/measure.sh` |

The source tree was clean at the recorded SHA. Both tests are ignored research
courts, so the ordinary suite compiles them without paying for the large
series. `cargo test -p tinyvm-qjs` was green at the same source state.

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
| C2 probe linearity | **pass** | every ASCII and UTF-8 four-point fit has 0.000000% measured residual |
| C3 ruler closure | **pass** | `10.5000 − 7.2500 = 3.2500` independently for both methods |
| C4 attribution closure | **pass** | `2.7500 + 1.2500 + 6.5000 + 0.0000 = 10.5000` exactly |
| C5 actionable owner | **pass: compare/branch** | 6.5000 steps/byte, above the frozen 0.50 threshold and larger than every other layer |
| C6 fixed overhead | **pass** | dispatch slope 0/intercept 76; full slope 10.5/intercept 84; P4 adds neither slope nor intercept on this clear-window series |
| C7 Unicode boundary | **pass** | byte, code-point and UTF-16-unit denominators are separated below |
| C8 reproducibility | **pass** | exact clean SHA, compiler, commands, raw totals and two source hashes recorded |

## Cumulative raw totals

| series | bytes | code points | UTF-16 units | build | dispatch | loop | read | compare | full |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| ASCII | 2,048 | 2,048 | 2,048 | 11,975 | 12,051 | 17,691 | 20,251 | 33,563 | 33,563 |
| ASCII | 8,192 | 8,192 | 8,192 | 43,191 | 43,267 | 65,803 | 76,043 | 129,291 | 129,291 |
| ASCII | 32,768 | 32,768 | 32,768 | 166,580 | 166,656 | 256,776 | 297,736 | 510,728 | 510,728 |
| ASCII | 131,072 | 131,072 | 131,072 | 658,635 | 658,711 | 1,019,167 | 1,183,007 | 2,034,975 | 2,034,975 |
| UTF-8 | 1,152 | 384 | 512 | 7,747 | 7,823 | 10,999 | 12,439 | 19,927 | 19,927 |
| UTF-8 | 4,608 | 1,536 | 2,048 | 25,523 | 25,599 | 38,279 | 44,039 | 73,991 | 73,991 |
| UTF-8 | 18,432 | 6,144 | 8,192 | 95,139 | 95,215 | 145,911 | 168,951 | 288,759 | 288,759 |
| UTF-8 | 73,728 | 24,576 | 32,768 | 372,141 | 372,217 | 574,977 | 667,137 | 1,146,369 | 1,146,369 |

## Attribution and denominator discipline

| layer | ASCII steps/byte | UTF-8 steps/byte | UTF-8 steps/code point | UTF-8 steps/UTF-16 unit |
|---|---:|---:|---:|---:|
| loop | 2.7500 | 2.7500 | 8.2500 | 6.1875 |
| four-byte load | 1.2500 | 1.2500 | 3.7500 | 2.8125 |
| has-zero compare + branch | **6.5000** | **6.5000** | **19.5000** | **14.6250** |
| exact verifier + miss | 0.0000 | 0.0000 | 0.0000 | 0.0000 |
| full absolute search | 10.5000 | 10.5000 | 31.5000 | 23.6250 |

These are separate views of each series, not cross-denominator ratios. The
UTF-8 seed has 9 bytes, 3 code points and 4 UTF-16 units.

## Verdict

The earlier direct-metadata verdict remains **REJECT** because its precommitted
court failed. The new evidence changes the interpretation of the numbers, not
that historical branch:

```text
old 7.2500 = absolute search 10.5000 − O(n) `.length` 3.2500
```

Therefore 7.2 → 10.5 was not a search-loop regression. The old subtraction is
not a valid cross-implementation ruler when `.length` itself changes from O(n)
to O(1). Future attribution and optimization courts must use the build-only
control.

The decision tree reaches a unique exit: the has-zero comparison and
clear-window branch own 6.5000 of 10.5000 steps/byte, so only that layer may
receive the next frozen optimization experiment. No engine optimization is
accepted by this attribution court.

## Deviations and honesty

- Before collecting the cumulative numbers, P1-P4 were renamed to match the
  actual production fast path: four-byte loop, `i32.load`, has-zero
  compare/branch, then exact verifier + miss. The earlier labels incorrectly
  described a byte decoder that this valid-UTF-8 search does not contain.
- The probes are checked-in `cfg(test)` code rather than transient patches.
  They cannot affect a production build or be selected through public API.
- P4 adds zero even to the fitted intercept because every complete window in
  this chosen series takes P3's clear-window branch and the lengths are exact
  multiples of four. That is a boundary of this court, not a universal claim
  that exact verification is free.
- No threshold, String layout, operation counter or production path was
  changed after observing the results. The result that comparison/branch—not
  memory load—is the dominant owner was accepted as measured.
