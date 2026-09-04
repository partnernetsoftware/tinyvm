# String record metadata: decisive experiment

Status: **completed; Variant B rejected and implementation rolled back**

Baseline: `f31dfcc` repository state; qjswasm engine code is unchanged from
`028a914`.

## Decision

Should a qjswasm module that uses positional string operations widen its
internal string record so UTF-16 length and the all-ASCII fact are available in
constant time, or should the engine retain the current four-byte record header
and its repeated UTF-8 walks?

This is a representation decision, not an invitation to add more String APIs.
The winner must improve real automation-script work without charging programs
that never use the affected operations or weakening any memory/load boundary.

## Why argument is exhausted

The current representation is compact and simple:

```text
[utf8_byte_length: i32][utf8 bytes][alignment padding]
```

It also makes `.length` O(n), ASCII `charCodeAt(i)` and `s[i]` O(i), and a
normal indexed loop O(n²). The existing eight-byte ASCII skip reduced the
constant but cannot change those slopes. Widening the record can flatten the
slopes, but it touches every producer and reader and adds four bytes to every
string created in an opted-in module. Neither side can win by further prose;
the decision needs a measured implementation court.

## Hard constraints

- JavaScript String length remains UTF-16 code units, not UTF-8 bytes or Unicode
  scalar values. `"😀".length == 2` stays true.
- Strings remain immutable and valid UTF-8 inside the guest.
- `HostParam::StrPtrLen` still exposes the UTF-8 body pointer and byte length;
  the internal record shape must not leak through the host door.
- Load validation, checked bump allocation, memory-page limits, typed heap
  exhaustion and the fault word remain unchanged in strength.
- One compiled module has one unambiguous string-record shape. No function may
  guess which layout a pointer uses.
- A program that cannot reach string length or positional string access must
  retain the current record and be byte-identical to the baseline output.
- The experiment may not raise step, memory, output, deadline or source limits.
- Existing correctness, adversarial heap, host-result and representation tests
  must remain green after their internal record readers are deliberately
  updated.

## Variants

### A — current record and scans

Keep the four-byte header. `.length` counts code units on every call;
positional access walks UTF-8 from the beginning, with the existing ASCII word
skip. This is the control.

### B — gated eager metadata

For modules whose compiler scan proves the metadata can be reached, use:

```text
[utf8_byte_length: i32][utf16_length + all_ascii flag: i32][utf8 bytes][padding]
```

The second word may pack the all-ASCII bit into the high bit because a valid
record's byte length and therefore its UTF-16 length are below `i32::MAX` after
header/allocation bounds. The implementation must prove that bound at every
constructor rather than rely on this paragraph.

Every string producer computes the word while it already has or creates the
bytes: literals, concatenation, slicing, case conversion, number conversion,
JSON parse/stringify and two-pass host byte results. `.length` masks and loads
the count. When all-ASCII is set, `charCodeAt(i)` and `s[i]` may address the
byte directly after the extended header; non-ASCII strings retain the current
UTF-8 walk and surrogate behavior.

Modules that cannot reach these operations keep Variant A. The gate must be
derived from the existing compiler scan and tested with positive and negative
programs; it is not a user option.

## Minimal experiment

Implement Variant B behind one crate-private representation choice. Do not add
a public feature flag or retain both variants after the verdict.

Measure A and B from clean builds on the same host and toolchain:

1. Compile and execute the existing `length_cost` and `char_access_cost`
   programs, recording exact VM steps.
2. Add 64- and 6,000-character points for repeated `.length`, and indices 3,
   63, 999 and 5,999 for ASCII `charCodeAt` and `s[i]`; report slopes, not only
   endpoints.
3. Exercise empty, ASCII, 2/3/4-byte UTF-8, mixed text and surrogate-boundary
   behavior through literals, concatenation, slice/substring, JSON and host
   byte results.
4. Compile a program that uses none of the affected operations and compare its
   wasm bytes exactly. Compile an opted-in corpus and report wasm-byte delta,
   interned-string count and predicted extra heap bytes.
5. Run a bounded `only_chars`-shaped workload using a 64-character identifier
   alphabet and a 40-byte candidate string. Record steps and wall time. Run the
   same workload at 10, 40, 160 and 640 bytes to expose the slope.
6. Run the complete `tinyvm-qjs` test suite, including `repr_v1`, `heap_attack`,
   JSON, conversions and host-argument/result tests.
7. After an upstream winner exists, pin it temporarily in AgenTerm and run the
   owning qjswasm crate tests plus one release-script journey. This downstream
   point confirms value; it may not repair or reinterpret an upstream failure.

The result table must contain, for both variants:

| Dimension | Required rows |
|---|---|
| correctness | ASCII, 2/3/4-byte, mixed, surrogate edge, every producer |
| cost | `.length`, `charCodeAt`, `s[i]`, full indexed loop, `only_chars` |
| slope | short/long length and four positional indices |
| code size | non-user exact bytes; opted-in wasm bytes and delta |
| memory | bytes per runtime string and representative peak pages |
| safety | load gate, hostile host length, heap exhaustion, malformed pointer |
| downstream | AgenTerm qjswasm tests and one real workflow receipt |

## Precommitted acceptance criteria

Variant B wins only if every Boolean gate and every quantitative gate passes.

### Boolean gates

1. All existing qjswasm correctness and adversarial tests pass.
2. All listed string producers initialize correct metadata before publishing a
   record; no consumer can observe an uninitialized or stale second word.
3. Non-participating programs compile to byte-identical wasm.
4. The external host door still receives identical UTF-8 pointer/length pairs.
5. Invalid host lengths, allocation overflow and memory exhaustion still fail
   through their existing typed/trapped boundary without memory corruption.

### Quantitative gates

1. Repeated `.length` on a 6,000-character string costs at most **160 steps per
   call**, and the per-call cost differs from the 64-character point by at most
   **16 steps**.
2. ASCII `charCodeAt(999)` and `s[999]` each cost at most **320 incremental
   steps** over the same build-only baseline.
3. Traversing all 1,000 ASCII positions with `charCodeAt` costs at most
   **600,000 incremental steps**. The current measured value is 3,009,139.
4. The `only_chars` workload improves by at least **3× in VM steps** at 160 and
   640 bytes, and its 640/160 per-byte cost ratio is at most **1.35**.
5. The opted-in representation adds exactly one four-byte word per string
   record and no second allocation. Representative AgenTerm workflow peak
   guest pages may grow by at most **5%**.
6. Opted-in wasm code/data growth is no more than
   `4 × string_record_count + 768` bytes relative to Variant A.

The constants above are frozen before Variant B measurements. A miss is a
result, not permission to move a threshold.

## Decision tree

```text
correctness, host ABI or heap safety fails?
├─ yes → reject B; keep A
└─ no
   ├─ non-participating wasm differs? → reject B; fix or abandon the gate
   └─ no
      ├─ length or ASCII-position slope misses? → reject B
      └─ pass
         ├─ only_chars benefit or size/memory cap misses? → keep A and test a
         │  compiler-loop/iterator experiment separately
         └─ all pass → accept B, delete experiment switch, document the new
            internal record invariant, then bump AgenTerm's exact pin
```

## Kill criterion

Stop Variant B immediately if it requires any of the following:

- two record layouts coexisting inside one compiled module;
- a public host ABI or `Value` representation change;
- trusting unvalidated host bytes or lengths;
- loosening allocator/load/memory limits;
- more than one extra allocation per created string; or
- task-specific shortcuts keyed to AgenTerm script names or contents.

## Time box

The experiment ends when both variants have a complete table for the seven
measurement steps above. No additional String methods, GC, rope representation,
interning policy, Unicode normalization or unrelated dispatcher optimization
may enter the time box.

## Excluded alternatives and non-answers

- Hoisting `s.length` out of a particular loop does not answer dynamic repeated
  reads or positional access and is therefore a separate compiler experiment.
- A mutable lazy cache adds a sentinel/write protocol to otherwise immutable
  strings and makes the first-call result depend on history; it is excluded
  unless eager metadata fails solely on measured construction cost.
- A side table adds lookup, identity and lifetime machinery to a no-free bump
  heap; it is outside this minimal comparison.
- Raising AgenTerm's operation budget, splitting a script, or replacing the
  workload with a host helper hides the engine cost and cannot count as a win.
- A faster microbenchmark without the downstream qjswasm journey is suggestive,
  not a verdict.

## Baseline results

Measured before implementation on the baseline above:

| Probe | Variant A |
|---|---:|
| `.length`, 6,000 ASCII, first call | 19,684 steps |
| `.length`, 6,000 ASCII, repeated call | 19,854 steps |
| `charCodeAt(999)`, 1,000 ASCII | 2,126 steps |
| `s[999]`, 1,000 ASCII | 2,153 steps |
| `substring(990)`, 1,000 ASCII | 2,645 steps |
| all 1,000 positions via `charCodeAt` | 3,009,139 steps |

Commands:

```sh
cargo test -p tinyvm-qjs --test length_cost --test char_access_cost -- --nocapture
```

## Variant B results and verdict

Measured on the same host and pinned toolchain from clean source states. The
Variant B measurements below were taken from the complete experimental diff
before that diff was rolled back. VM steps, rather than wall time, are the
decision authority; wall time was recorded for the `only_chars` rows as a
diagnostic but varied with host scheduling.

| Probe | Variant A | Variant B | Frozen gate |
|---|---:|---:|---|
| repeated `.length`, 64 ASCII | 538/call | 166/call | B long ≤160; short/long Δ≤16 |
| repeated `.length`, 6,000 ASCII | 19,830/call | **166/call** | **fail: 166 > 160** |
| `charCodeAt(3)` | 356 | 82 | slope evidence |
| `charCodeAt(63)` | 880 | 82 | slope evidence |
| `charCodeAt(999)` | 5,560 | 82 | pass: ≤320 |
| `charCodeAt(5,999)` | 30,560 | 82 | slope evidence |
| `s[3]` | 421 | 164 | slope evidence |
| `s[63]` | 945 | 164 | slope evidence |
| `s[999]` | 5,625 | 164 | pass: ≤320 |
| `s[5,999]` | 30,587 | 164 | slope evidence |
| all 1,000 positions, `charCodeAt` | 3,009,139 | 228,053 | pass: ≤600,000 |
| `only_chars`, 10 bytes | 24,712 | 6,957 (3.55×) | diagnostic |
| `only_chars`, 40 bytes | 103,221 | 27,507 (3.75×) | diagnostic |
| `only_chars`, 160 bytes | 522,471 | 109,707 (4.76×) | pass: ≥3× |
| `only_chars`, 640 bytes | 3,855,471 | 438,507 (8.79×) | pass: ≥3× |
| B `only_chars` 640/160 per-byte ratio | — | 1.00 | pass: ≤1.35 |

The B court also established these Boolean facts before the failing gate ended
the experiment:

- empty, ASCII, 2/3/4-byte UTF-8, mixed text and the two sides of a surrogate
  pair remained correct;
- literals, concatenation, trim, slice/substring, case conversion, decimal
  number conversion, JSON parse and JSON stringify produced correct metadata;
- the external `StrPtrLen` door still received the body pointer and UTF-8 byte
  length rather than the widened header;
- the chosen module carried one explicit record layout and a module marker;
  public fault-detail decoding did not infer a layout from arbitrary record
  bytes;
- the non-participating program `let x = 40; return x + 2;` was exactly 10,232
  wasm bytes under both A and B source states.

The complete-suite rehearsal was intentionally not converted into new frozen
price pins after the quantitative failure. It reached 33 failing integration
test targets: the semantic failures observed were old test-side four-byte
record readers, while the remainder were exact-size or historical lower-bound
cost assertions invalidated by the experimental representation. Updating all
of those witnesses, measuring opted-in wasm growth/string count and measuring
representative peak pages cannot rescue the already-failed 160-step gate, so
the decision tree stops before those promotion-only measurements. They are
therefore recorded as **not reached**, not as passes.

### Verdict

**Reject Variant B and keep Variant A.** The widened record flattened every
target slope and passed the positional and `only_chars` thresholds, but its
best general `.length` lowering cost 166 VM steps per call. The precommitted
limit is 160 and is not movable after measurement. Per the decision tree, that
single quantitative miss is decisive. All compiler/runtime/test changes for B
were rolled back; this result section is the only retained experiment change.

A later experiment may attack the generic member-access/lowering overhead or
introduce a loop/iterator optimization, but it must be specified separately.
It must not relabel 166 as passing this experiment and must retain Variant A's
four-byte String record until another precommitted court selects a replacement.
