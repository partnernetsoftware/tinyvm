# String search XOR-lowering court

Owner specification:
[`plan/design-string-search-xor-lowering-experiment.md`](../../plan/design-string-search-xor-lowering-experiment.md).

The court compares the checked-in arithmetic XOR spelling with direct Wasm
`i32.xor`, using the existing build-only search ruler. It must preserve the
same four-byte has-zero algorithm and public semantics.

From repository root:

```sh
./research/string-search-xor-lowering/measure.sh
```

Accepted numbers, exact source identity and the decision trace belong in
`RESULTS.md`; raw Cargo output is not committed.
