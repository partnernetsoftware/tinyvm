# String search attribution court

Owner specification:
[`plan/design-string-search-cost-attribution-experiment.md`](../../plan/design-string-search-cost-attribution-experiment.md).

This directory holds reproducible evidence, not a second implementation. The
first landed court measures the two rulers that must be settled before adding
P0-dispatch through P4-miss diagnostic probes:

```text
same generated haystack
├─ build-only        return 0
├─ historical        return s.length
├─ full includes     absent needle
└─ full indexOf      absent needle
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
operation counter. P1-P4 remain unimplemented until their cumulative probes can
preserve the production search skeleton required by the specification.
