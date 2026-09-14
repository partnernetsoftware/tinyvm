# Host-binding monomorphization: decisive experiment

Status: **decided — rejected**

Date: 2026-09-14–15
Baseline: agenterm `c642be46`; tinyvm `9b943e7`
Purpose: decide whether the per-call-site `bind_import_typed` monomorphization
is a measurable share of a downstream binary's `.text`, and whether moving the
post-erasure scan/install loop into a private non-generic helper removes a
measurable part of it without changing allocation count, runtime call depth or
public API.
Implementation location if opened: `crates/tinyvm/src/wasm.rs` plus its owning
tests only. No public feature, no ABI change, no AgenTerm edit.
Pre-reading: `prd/notes-performance.md` (the closure-generic precedent and the
open function-pointer question).

This is an upstream engine experiment. It does not change AgenTerm capability
status, version scope or the tinyvm pin unless every gate selects the candidate.

## 0. Decision and settled facts

Should the scan/install loop that runs **after** a typed host callback has
already been erased into `Rc<TypedHostImpl>` live in the generic
`bind_import_typed<F>` body, or in a private non-generic helper the generic body
calls once?

Already settled, and not re-opened here:

1. `prd/notes-performance.md` records the same mechanism in the static core:
   closure generics across ~150 call sites grew the core from 101,240 to
   117,752 bytes (+16,512 B, ~110 B/call site), while a non-generic shared
   function added only +16 bytes. It also records the **open** question that a
   function pointer adds one indirect call and must be measured at the gate.
   Those numbers are precedent and cross-calibration only; they are not this
   experiment's result.
2. The downstream caller (agenterm's `agenterm-qjswasm`) declares **11**
   `HostFn` rows and has **66 closure-producing binding call sites**: 53
   `bind_metered` calls in `tool.rs`, 11 direct `bind` calls in `host.rs` and
   2 direct `bind` calls in `tool.rs`. These produce **66 distinct closure
   types**. The **211** figure is a symbol/inlining instance count, not a
   call-site count, and must not be quoted as one.
3. In the current source the generic part of `bind_import_typed<F>` is exactly
   one line — `Rc::new(move |args, _n_results, memory| f(args, memory))` — and
   the loop that follows already operates on `Rc<TypedHostImpl>` and names no
   `F`. The same shape holds for `bind_import_typed_in_place<F>`, whose generic
   part is `Rc::new(f)`.
4. `bind_import_typed` is deliberately kept monomorphic in behaviour: the
   returned-`Vec<Val>` path is an explicit arbitrary-arity compatibility door,
   and latency-sensitive hosts are documented to prefer
   `bind_import_typed_in_place`.

## 1. Hard constraints

- Public API is frozen: no signature, name, visibility or error-variant change
  to `bind_import_typed`, `bind_import_typed_in_place`,
  `bind_import_typed_in_place_with_memories`, `HostBinding`, `TypedHostImpl` or
  `WasmError`.
- **Allocation count must not increase.** One `Rc<TypedHostImpl>` is allocated
  per bind call today; the candidate allocates exactly the same one.
- **Runtime call depth must not increase.** The callback still dispatches
  through `Rc<TypedHostImpl>`; no extra indirection layer may be introduced at
  call time. The helper runs at bind time only.
- **Multiple same-named import semantics are preserved**: every matching slot
  binds to the same implementation, and a bind that reports success never
  leaves a sibling unbound.
- Error semantics preserved exactly: a missing `module.field` still yields
  `WasmError::Trap("no imported function named")` for `bind_import_typed`, and
  the `_in_place` family keeps its own pre-scan arity check and its own
  `found`-based error path.
- Existing owning courts must keep passing unchanged.
- No edit to the AgenTerm repository.

## 2. Minimal experiment content

| Dimension | Choice | Why |
|---|---|---|
| Site under test | `bind_import_typed` in `crates/tinyvm/src/wasm.rs` | This is the compatibility door used by the measured downstream bindings |
| Variant A | current source, unmodified | the control |
| Variant B | post-erasure loop moved into a private non-generic helper taking `Rc<TypedHostImpl>` | isolates that loop from the generic wrapper without touching the bounded family |
| Downstream harness | AgenTerm build with `[patch."https://github.com/partnernetsoftware/tinyvm"] path = <local tinyvm>` | the git dependency cannot be patched by `[patch.crates-io]` |
| Slope probe | add 10 same-signature host ops downstream and rebuild both variants | a single point measures no slope |
| Timing probe | fixed-iteration host-call guest, median of repeated runs | answers the recorded function-pointer question |

## 3. Criteria and measurement discipline

**Measurement discipline.** Every byte figure carries a four-part label:
`{boundary, tool, build, target/execution}`.

- Boundary: the downstream `agenterm` binary's `.text`, including all symbols
  attributed to `tinyvm` and `tinyvm_qjs`; excludes other `__TEXT` sections and
  `__DATA`.
- Tool: `cargo bloat` (DWARF/crate attribution). On macOS, `nm -S` reports a
  zero size for every symbol (Mach-O `nlist_64` carries no size), and
  `otool -v -t` address differencing mis-attributes inlined instances to
  `tinyvm`; neither may be used as the primary口径.
- Build: `profile.release` = `opt-level="z"`, `lto="thin"`,
  `codegen-units=1`, `panic="abort"`, plus
  `--config 'profile.release.strip="none"'` so attribution is possible.
  `strip` affects only the symbol table, not optimisation or linking.
- Target/execution: macOS arm64 release. Timing numbers are taken by real
  execution; byte numbers are taken from a build that is **not executed**.

| # | Criterion | Nature | Gate |
|---|---|---|---|
| 1 | no increase in `Rc` allocations per bind call | boolean | must pass |
| 2 | no increase in runtime call depth | boolean | must pass |
| 3 | public API / errors / same-name import semantics unchanged | boolean | must pass |
| 4 | marginal `.text` per 10 added same-signature host ops | **slope** | must fall by ≥50% vs A |
| 5 | current downstream `.text` net reduction | intercept | ≥32 KiB |
| 6 | host-call latency | safety | must not regress by >2% |

**Acceptance requires all applicable gates to pass.** The original measurement
order was 4 (slope) → 5 (intercept) → 6 (latency). After the first A/B showed a
zero intercept, the coordinator amended the time box to measure criterion 5
first and exit if it missed 32 KiB. This does not change the acceptance set — a
candidate failing mandatory criterion 5 could never be selected — but it does
change which expensive measurement runs first and is therefore recorded as a
specification deviation in §8.3.

## 4. Decision tree, kill criterion, time box

```
1. Cheap eligibility gate = criterion 5 (intercept). If B does not reduce the
   current downstream .text by >=32 KiB -> reject B without manufacturing ten
   downstream operations solely to measure slope.
2. Criterion 5 passes -> main gate = criterion 4 (slope). B's marginal .text
   per 10 same-signature host ops must fall at least 50% below A's.
3. Criteria 5 and 4 pass -> look at criterion 6 (latency), which must not
   regress by more than 2%.
Boolean gate: if any of 1, 2 or 3 fails -> the candidate is invalid immediately,
   independent of 4 and 5.
kill criterion: criterion 5 below 32 KiB rejects the candidate immediately; if
   it passes, criterion 4 below 50% rejects it. No later criterion rescues a
   failed earlier mandatory gate.
time box: measure criterion 5 first. Only if it passes, create the ten-operation
   slope fixture and measure criterion 4; only if both pass, measure criterion 6.
```

Every criterion above appears in the tree: 1–3 as the boolean gate, 5 as the
cheap eligibility node, 4 as the main slope node for surviving candidates and
6 as the final latency node.

## 5. Directory structure

- Specification: this file.
- Attribution evidence: `prd/notes-performance.md` (status section added).
- A/B artefacts: build outside both repositories; no generated artefact is
  committed.

## 6. Excluded options

| Option | Why excluded |
|---|---|
| Replace the closure with a `fn` pointer in the public signature | changes the public API and adds an indirect call the gate has not yet cleared |
| Erase `bind_import_typed_in_place`'s bounded binding too | separate binding family; would widen the change without evidence |
| Change the downstream caller to call an existing non-generic door | a downstream edit; this experiment is upstream-only |
| Deduplicate the 66 closure types by rewriting call sites | downstream change, and the types are genuinely distinct closures |

## 7. What this experiment does not answer

- Whether the same sharing helps `bind_import_typed_in_place_with_memories` or
  the `adapt_i32_host` legacy door.
- Whether downstream call-site count can be reduced architecturally.
- Whether any other embedding (wbox and others) sees the same slope.
- Whether the `staticcore` build (no `std`) shows the same ladder; that build
  was the precedent's subject, not this experiment's.

## 8. Conclusion (written after the A/B)

**Verdict: rejected.** The candidate's intercept is 0 KiB, which triggers the
early exit in §4 before the slope and latency criteria are ever reached.

Because criterion 5's intercept reduction is below the 32 KiB gate, §4 sends
the decision straight to the negative branch: **criteria 4 (slope) and 6
(latency) were never measured.** This experiment therefore states nothing about
the slope or the latency of this shape — not that the slope failed, and not
that latency regressed. Both are **not reached**.

### 8.1 Decision trace, walked through §4's tree

1. Cheap eligibility gate = criterion 5 (intercept). Measured: **0 KiB** net
   reduction against a 32 KiB gate. **Fails.**
2. Per §4, a failed intercept exits to the negative branch without creating the
   synthetic slope fixture. **Criterion 4 not reached. Criterion 6 not reached.**
3. Boolean gate (criteria 1, 2, 3): all three hold. Allocations per bind call
   are unchanged (one `Rc::new` per bind); no runtime call layer is added (the
   helper runs at module-bind time only, never during a guest call); public
   API, error values and the same-name multiple-import semantics are byte-for-
   byte the same behaviour, and the whole tinyvm workspace suite passes.

### 8.2 Numbers

口径 = `{boundary, tool, build, target/execution}`:

- **boundary**: the downstream `agenterm` binary's `.text`, including every
  symbol attributed to `tinyvm` and `tinyvm_qjs`; excludes other `__TEXT`
  sections and `__DATA`.
- **tool**: `cargo bloat` (DWARF/crate attribution). `nm -S` is unusable on
  macOS (Mach-O `nlist_64` carries no size) and `otool -v -t` address
  differencing mis-attributes inlined instances, so neither is the primary
  ruler.
- **build**: `profile.release` = `opt-level="z"`, `lto="thin"`,
  `codegen-units=1`, `panic="abort"`, plus
  `--config 'profile.release.strip="none"'`. `strip` affects only the symbol
  table.
- **target/execution**: macOS arm64 release, **byte measurements only; the
  measured build was never executed.**

| Quantity | A (tinyvm at HEAD) | B (helper + `inline(never)`) | Net |
| --- | ---: | ---: | ---: |
| `bind_import_typed` symbols | 211 / 69.7 KiB | 211 / 69.7 KiB | +0.0 KiB |
| `tinyvm` crate | 140.5 KiB | 140.5 KiB | +0.0 KiB |
| total `.text` | 5133.1 KiB | 5133.1 KiB | +0.0 KiB |
| binary file | 10,108,664 B | 10,108,648 B | −16 B (alignment/noise) |

Helper symbol survival: **none.** `nm`, `strings` and a mangled-name fragment
search all report 0 occurrences of `install_typed_returning` in the final
binary, and the same search inside tinyvm's own rlib and its extracted object
finds nothing either.

### 8.3 Deviations from the specification (all of them)

1. **First A/B, no attribute.** B measured identical to A in every line, and
   the helper produced no attributable symbol. The build may have inlined,
   internalised or merged it; this experiment does not distinguish those
   optimiser actions.
2. **Second A/B, `#[inline(never)]` added.** Net reduction still 0 KiB and the
   helper still did not survive as an independent symbol. So the shape is
   unproductive **with or without** the attribute under this build.
3. **Two measurement corrections.** (a) Attribution was first attempted with
   `otool -v -t` address differencing, which reported `tinyvm` 156.8 KiB and
   `tinyvm_qjs` 247.0 KiB — systematically ~13% high because agenterm-inlined
   generic instances were counted as tinyvm. `cargo bloat` gives 140.5 KiB and
   216.1 KiB and agrees with the figure recorded earlier in
   `prd/notes-performance.md`. (b) An intermediate report claimed `nm` had
   found 1 `install_typed_returning` symbol; the same command re-run, plus
   `strings` and a strict mangled-fragment search, all returned 0. That report
   was a misread echo; the correct value is **0**.
4. **Scope narrowed mid-experiment.** `install_typed_bounded` and the
   `bind_import_typed_in_place` rewrite were reverted at the coordinator's
   direction, because the downstream 69.8 KiB lesion only travels through
   `bind_import_typed`/`TypedReturning`. The `_in_place` family is untouched.
5. **One `git stash push` was used** to switch the A baseline build before the
   coordinator's no-stash instruction. Its residue (`stash@{0}`, 39 lines) left
   the working tree at HEAD for a period; it was restored with
   `git stash pop` and verified identical. No further stash or checkout was
   used.
6. **Measurement order amended after the first A/B.** The original tree put
   slope before intercept. The coordinator moved the independently mandatory
   32 KiB intercept gate first, before the final A/B, so a zero-benefit
   candidate would not require ten synthetic downstream operations. This
   changed execution cost, not which candidates could be accepted.

### 8.4 Honesty clause

No measurement was changed to make the conclusion look better. The verdict is
negative and it is stated as such. Two readings are **not** available here: the
slope criterion was never reached, so no claim is made about it either way.

### 8.5 What surprised us

The most valuable finding is negative and reusable: **this bind-time private
helper produces no attributable code-size reduction under the measured thin-LTO
build, with or without `#[inline(never)]`.** The helper does not survive as a
named symbol, but this experiment does not distinguish inlining from
internalisation or identical-code merging. A future attempt at the same shape
should not be expected to pay off without new evidence.

That also falsifies the scan/install loop as a measurable, separately
extractable source of the 211 instances / 69.7 KiB under this build. It does not
uniquely attribute all remaining bytes. Testing the generic boundary itself —
for example an additive entry accepting an already-erased `Rc` or a function
pointer — would be a separate public-API and latency experiment outside these
prior constraints.

### 8.6 Scope of the rejection

This experiment rejects exactly one shape: **extracting the post-erasure
scan/install loop into a private non-generic helper.** It does not reject:

- a future experiment on the public generic boundary itself, once new evidence
  exists (for example a measured indirect-call cost that clears the latency
  gate);
- `bind_import_typed_in_place` or the bounded families, which this experiment
  never touched;
- the `staticcore` build, which was the precedent's subject and not this
  experiment's.

### 8.7 Reproduction commands

From the AgenTerm repository root, with the tinyvm repository in the sibling
`../tinyvm` directory, build the downstream B variant without editing
AgenTerm's manifest:

```
CARGO_TARGET_DIR=target/tv-AB cargo build --profile release --bin agenterm \
  --config 'profile.release.strip="none"' \
  --config 'patch."https://github.com/partnernetsoftware/tinyvm".tinyvm.path="../tinyvm/crates/tinyvm"' \
  --config 'patch."https://github.com/partnernetsoftware/tinyvm".tinyvm-qjs.path="../tinyvm/crates/tinyvm-qjs"'
```

Confirm the patch resolved to the local path (without the `--config` flags the
tree still shows the pinned revision):

```
CARGO_TARGET_DIR=target/tv-AB cargo tree -i tinyvm --config 'patch."https://github.com/partnernetsoftware/tinyvm".tinyvm.path="../tinyvm/crates/tinyvm"' --config 'patch."https://github.com/partnernetsoftware/tinyvm".tinyvm-qjs.path="../tinyvm/crates/tinyvm-qjs"'
```

Attribute:

```
CARGO_TARGET_DIR=target/tv-AB cargo bloat --profile release --bin agenterm -n 20 --crates
CARGO_TARGET_DIR=target/tv-AB cargo bloat --profile release --bin agenterm -n 0 --symbols
```

Independent reference values: binary `10,108,664 B` for A and `10,108,648 B`
for B; `cargo bloat --crates` reports `tinyvm` = 140.5 KiB and `tinyvm_qjs` =
216.1 KiB in both.

Own test suite, excluding the simulator-dependent test:

```
cargo test --workspace -- --skip ios_wasi_host_simulator_container
```
