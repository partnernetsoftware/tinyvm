# String search attribution court

Owner specification:
[`plan/design-string-search-cost-attribution-experiment.md`](../../plan/design-string-search-cost-attribution-experiment.md).

This directory holds reproducible evidence, not a second implementation. The
court first closes the two historical rulers, then measures cumulative,
test-only cuts of the production `includes` miss body:

```text
same generated haystack
├─ build-only        return 0
├─ historical        return s.length
├─ full includes     absent needle
├─ full indexOf      absent needle
└─ cumulative body
   ├─ dispatch/setup
   ├─ four-byte position loop
   ├─ production i32.load
   ├─ has-zero-byte comparison + clear-window branch
   └─ exact verifier + miss result
```

Run from repository root:

```sh
./research/string-search-cost-attribution/measure.sh
```

The ordinary test suite compiles this court but leaves its expensive
measurement ignored. The script is the explicit evidence entry. Raw output is
printed as CSV rows; accepted aggregates and the exact source identity belong
in `RESULTS.md`.

No result here changes the production String record, an existing gate or the
operation counter. The cumulative cuts exist only under `cfg(test)` and are
selected through a thread-local compile hook, so ordinary and production
compiler builds have no diagnostic mode or public API. ASCII and UTF-8 series
report byte, code-point and UTF-16-code-unit denominators separately.
