# String search XOR-lowering results

Status: **decided — direct `i32.xor` accepted**.

## Identity

| Field | Value |
|---|---|
| Measurement SHA | `17db24d38b4ac0f11f9c58fe32b6a86e894d0ad2` |
| Method/court SHA-256 | `b64638ec983e288926cbe5ca7203169e635cfc17efdfc8add8f6dbb7e4bdeb69` |
| Semantic cost-test SHA-256 | `65699870d8417cda34447071f9222185d81e64d66522e188bce7d00b85603957` |
| Rust / LLVM | `rustc 1.97.0 (2d8144b78 2026-07-07)` / `22.1.6` |
| Host | `aarch64-apple-darwin` |
| Command | `CARGO_TARGET_DIR=target/search-xor-lowering ./research/string-search-xor-lowering/measure.sh` |

The measurement tree was clean. The test-only variant selector compiled both
lowerings from the same source state; production defaults to direct XOR and
exposes no selector.

## Slopes and module bytes

| series | method | arithmetic steps/byte | direct steps/byte | improvement | arithmetic module | direct module | delta |
|---|---|---:|---:|---:|---:|---:|---:|
| ASCII | `includes` | 10.5000 | **9.5000** | 1.0000 | 11,175 | 11,169 | -6 B |
| ASCII | `indexOf` | 10.5000 | **9.5000** | 1.0000 | 11,254 | 11,248 | -6 B |
| UTF-8 | `includes` | 10.5000 | **9.5000** | 1.0000 | 11,171 | 11,165 | -6 B |
| UTF-8 | `indexOf` | 10.5000 | **9.5000** | 1.0000 | 11,250 | 11,244 | -6 B |

All slopes are build-only-subtracted least-squares fits across four lengths.
ASCII and UTF-8 intercepts are unchanged: 84 for `includes`, 85 for `indexOf`.
UTF-8 is compared to ASCII only per byte; its code-point and UTF-16-unit views
remain separate in the owner attribution result.

At the longest points the direct totals were:

| series | method | bytes | build | arithmetic | direct |
|---|---|---:|---:|---:|---:|
| ASCII | `includes` | 131,072 | 658,635 | 2,034,975 | 1,903,903 |
| ASCII | `indexOf` | 131,072 | 658,635 | 2,034,976 | 1,903,904 |
| UTF-8 | `includes` | 73,728 | 372,141 | 1,146,369 | 1,072,641 |
| UTF-8 | `indexOf` | 73,728 | 372,141 | 1,146,370 | 1,072,642 |

## Criteria and decision trace

| Criterion | Result | Evidence |
|---|---|---|
| X0 semantics | **pass** | focused hit/miss/window tests green; full tinyvm-qjs suite green after updating exact size pins |
| X1 absolute slope | **pass** | all four direct rows are 9.5000, strictly below 10.0 |
| X2 improvement | **pass** | all four improve by 1.0000, above 0.75 |
| X3 boundary | **pass** | ASCII and valid UTF-8 per-byte slopes are identical |
| X4 size | **pass** | every emitted module is 6 bytes smaller |
| X5 reproducibility | **pass** | clean SHA, compiler, command, source hashes and raw longest points recorded |

Decision: X0 pass → X1/X2/X3 pass → X4 pass → **ACCEPT direct XOR**.

## Deviations and honesty

- Exact-size tests correctly failed first because they pinned the old module
  size. Their expected values were reduced by exactly 6 bytes; no tolerance was
  introduced.
- The production source change is one identity spelling. The checked-in
  arithmetic baseline exists only under `cfg(test)` so both variants remain
  reproducible without reverting source.
- The win is exactly the predicted four fewer interpreted instructions per
  four-byte window. No threshold, counter, String layout or unrelated loop
  instruction changed after measurement.
