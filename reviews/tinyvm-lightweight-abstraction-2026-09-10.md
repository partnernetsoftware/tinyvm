# tinyvm lightweight abstraction review

**Date:** 2026-09-10 · **Mode:** read-only architecture review · **Scope:** `crates/tinyvm` kernel (not implementation)

---

## Snapshot tip / SHA

| Field | Value |
| --- | --- |
| **Tip SHA** | `b4883be5cfe6f12702549a059dac2fcb85fd5688` |
| **Tip subject** | `wasm: interrupt start-section instantiation` |
| **Workspace** | `tinyvm` + `tinyvm-qjs` (review focuses on kernel crate) |
| **Product face (unchanged)** | `eval_wasm(data, globals, locals)` · `Module::from_bytes*` · `Module::instantiate` · `Instance::invoke*` |

Recent tip adds cooperative interrupt during **start-section instantiation** (`instantiate_with_interrupt`): failed/interrupted start calls `unregister_instance` and returns no `Instance`.

---

## Hot path map

### Crate / module layout

| Layer | Path | LOC (approx) | Role |
| --- | --- | ---: | --- |
| **Public kernel** | `crates/tinyvm/src/wasm.rs` | 11,976 | Decode · validate gate · `Module`/`Instance` · interpreter · host bindings |
| **Load validator** | `crates/tinyvm/src/wasm/validate.rs` | 1,019 | Abstract-stack validation over decoded `Op` |
| **Guest memory helpers** | `crates/tinyvm/src/wasm/guest_memory.rs` | 107 | `(ptr,len)` window checks for host callbacks |
| **Game embedding** | `crates/tinyvm/src/game.rs` | 1,537 | Cartridge policy · `instantiate` · tick lifecycle |
| **Legacy toy VM** | `crates/tinyvm/src/lib.rs` (`Vm`/`Instr`) | 757 total | Pre-wasm compatibility / tests; not product face |
| **Host / cartridge / replay** | `host.rs`, `cartridge.rs`, `replay.rs`, … | ~2k | Platform doors; not on eval hot path |
| **Language skin** | `crates/tinyvm-qjs` | separate crate | `eval_qjs = eval_wasm(qjs2wasm(src)?, …)` |

> ~46% of `tinyvm` source lines live in a single `wasm.rs` translation unit.

### End-to-end pipeline

```mermaid
flowchart LR
  bytes[".wasm bytes"]
  decode["decode() section loop"]
  body["decode(body) → Op[]"]
  gate["validate_body() × functions"]
  mod["Module (immutable template)"]
  inst["instantiate / invoke bootstrap"]
  run["call_any → run_defined Op loop"]
  tear["drop Instance / ephemeral stack"]

  bytes --> decode --> body --> gate --> mod --> inst --> run --> tear
```

| Phase | Entry | Core work | Output / side effects |
| --- | --- | --- | --- |
| **1 Load / decode** | `Module::from_bytes_with` → `decode` | Section parse; `parse_code_section`; per-body `decode()` → `Vec<Op>` + `branch_targets`; table minima materialized into `TableDesc.elements` | `Module` or `WasmError::Decode` |
| **2 Validate (load gate)** | end of `decode` | Build `validate::ModuleCtx`; `validate_body` walks every `Op` | Invalid module never exported |
| **3 Instantiate** | `Module::instantiate[_with_interrupt]` → `Instance::new` | `execution_store`; `allocate_instance_id`; `new_globals` / `new_memories` / `new_table_state`; strip import handles; `register_instance_state`; optional **start** via `Store::invoke_registered` | `Instance` + live memory/globals/tables |
| **4 Execute** | `Instance::invoke_val*` **or** ephemeral `Module::invoke_val` / `eval_wasm` | `Store::invoke_registered` (persistent) **or** direct `call_any` (ephemeral); `run_defined` dispatches `Op` | `Vec<Val>` / trap |
| **5 Teardown** | `Drop` / scope end | Ephemeral: locals dropped; persistent: `Instance` dropped — **store slot not cleared** (see findings) | Memory reclaimed only when last `Rc` dies |

### Call-site variants (same kernel, different lifetimes)

| API | State lifetime | Start fn | Store registration |
| --- | --- | --- | --- |
| `eval_wasm` → `Module::eval` | Ephemeral per call | Yes, then entry export | `allocate_instance_id` only — **not registered** |
| `Module::invoke_val` | Ephemeral per call | No | Same leak pattern |
| `Module::instantiate` | Persistent `Instance` | Yes (interruptible) | Registered; failed start unregisters |
| `Instance::invoke_val` | Reuses instance | No | Uses registered state |

---

## Abstraction / reuse findings

| ID | Finding | Leak / cost | Notes |
| ---: | --- | --- | --- |
| A1 | **Monolithic `wasm.rs`** folds wire decode, IR (`Op`), validation orchestration, instantiation, interpreter, and host ABI into one file | Maintenance drag; poor icache locality on embedded targets | `validate.rs` and `guest_memory.rs` are the only wasm splits |
| A2 | **Triple `Op` dispatch** — `decode()` match, `validate::step` match, `run_defined` match (~400 opcode arms × 3) | Large code footprint; any opcode change touches 3 sites | No shared opcode metadata table |
| A3 | **Parallel table models** — `TableDesc { elements: Vec<Val> }` (module template) vs `TableSlot { elements: Vec<TableElement> }` (instance) | Concept duplicated; conversion loop in `new_table_state` | Imported tables partially avoid template allocation |
| A4 | **Instantiation bootstrap copied 3×** — `new_globals` + `new_memories` + `new_data_state` + `new_table_state` wired in `Module::eval`, `invoke_val_controlled`, `Instance::new` | Bug surface (start/interrupt only on some paths); hard to keep ceilings consistent | Recent interrupt work touched `Instance::new` only |
| A5 | **Host call funnel split** — `call_host`, `call_host_with_memories`, `adapt_i32_host`, four `HostBinding` variants | Repeated arity/type checks and result buffering | Bounded vs owned paths differ mostly by allocation |
| A6 | **`Module` embedded in `InstanceState`** — full decoded module (all `Func.code`, data/elem segments, export maps) lives inside each instance | RSS multiplier for N instances of same cartridge | No `Arc<Module>` / module-id indirection |
| A7 | **Legacy `Vm`/`Instr`** cohabits `lib.rs` with wasm product | Confuses “kernel” mental model; still compiled in normal builds | Gated from `staticcore` self-test path only |
| A8 | **Inspect path re-walks code** — `CartridgeDescriptor::inspect` → full `from_bytes`; `feature_usage()` scans all `Op`s again | Extra CPU on catalog/inspect flows | Metadata could be captured at load gate |
| A9 | **i32 convenience wrappers** — `i32_args_to_vals` / `vals_to_i32` duplicated at every `invoke` entry | Minor alloc churn on i32-only embedders | Acceptable for face compatibility |

---

## Lightweight (CPU / mem) findings

| ID | Finding | Symptom | Severity |
| ---: | --- | --- | --- |
| L1 | **Store `instance_id` leak on ephemeral paths** — `Module::eval` / `invoke_val_controlled` call `allocate_instance_id` but never `register_instance_state` or release slot | `StoreState.instances` grows by one `None` entry per eval/invoke; monotonic RSS in REPL / `eval_wasm` loops | **High** |
| L2 | **Table double allocation** — load fills `TableDesc.elements` (`Vec<Val>`); instantiate clones into `TableSlot` (`Vec<TableElement>`) | ~2× table min memory while module + instance alive; extra copy at bootstrap | **High** |
| L3 | **`Func.locals` stores `Vec<Val>` zeros** not type bytes — activation does `extend_from_slice(&func.locals)` | Extra memory per function in `Module`; wider copies at every call setup | **Med** |
| L4 | **`function_type()` allocates** fresh `FuncType` vecs when signature missing; **`call_indirect` clones** expected type each check | Allocator churn on indirect-heavy guests | **Med** |
| L5 | **No `Drop for Instance`** — successful instantiate registers `Rc<InstanceState>` in store; dropping `Instance` does not `unregister_instance` | Instance memory retained until `Store` dropped; blocks reuse of instance-id space | **Med** |
| L6 | **`RefCell` + `Rc` on hot path** — every store table access, global get/set, cross-instance call borrows | Atomic/ref-count overhead vs single-owner embed | **Med** (needed for linked imports today) |
| L7 | **Load-time work on inspect** — `from_bytes_with` decodes + validates entire module for descriptor-only queries | CPU on `cartridge check` / compatibility scans | **Med** |
| L8 | **`branch_targets` side table + large `Op` enum** — `br_table` targets stored per function; enum includes SIMD/host variants behind features | Decode memory ∝ functions × (ops + branch fanout) | **Low–Med** |
| L9 | **Interrupt poll every 1024 steps** in `run_defined` | Predictable; acceptable | **Low** (by design) |
| L10 | **`panic = "abort"` release profile** | Smaller binary; no unwind cost | **Positive** |

### Abort / fault paths (lightweight-relevant)

- Load failures → `WasmError::Decode` (cheap static strings; no fmt).
- Runtime → `WasmError::Trap` (static str); cooperative interrupt → `Trap("interrupted")`.
- Start failure after partial side effects → instance unregistered (tip commit); **not** a full transaction rollback (documented).
- Allocator failure → explicit `try_reserve` / trap strings (no OOM abort in kernel).

---

## Ranked cut tree

Legend: `[ ]` not started · `[~]` partial / needs design · `[x]` done

Priority = **impact on CPU/RSS ÷ product-face risk** (lower index = do first). All cuts preserve the public eval/instantiate face.

```
P0 — correctness-adjacent RSS (hot eval path)
[ ] 1. Ephemeral eval/instance-id hygiene
    [ ] Stop calling allocate_instance_id on Module::eval / invoke_val OR register+unregister in a RAII guard
    [ ] Add regression test: N× eval_wasm does not grow Store.instances.len()
[ ] 2. Table template slimming
    [ ] Module.tables stores (element_type, min, max, imported) only — no Val[] template
    [ ] Single null-fill at new_table_state (one Vec<TableElement> per instance)
[ ] 3. Shared instantiation bootstrap (internal)
    [ ] Extract fn bootstrap_ephemeral(module, start: bool) used by eval + invoke_val
    [ ] Extract fn bootstrap_persistent used by Instance::new (start + interrupt flag)
    [ ] One place for Limits checks (memory pages, table elems)

P1 — instance lifecycle RSS
[ ] 4. Instance teardown
    [ ] impl Drop for Instance → store.unregister_instance(instance_id) when last external handle gone
    [ ] Document Store lifetime vs Instance lifetime for embedders
[ ] 5. Module archetype sharing
    [ ] Module behind Arc (or module-id pool) in InstanceState; drop duplicated segment bytes per instance
    [ ] Instance overlay holds only data_live, elem_live, memories, globals, tables

P2 — interpreter / load CPU & code size
[ ] 6. Locals representation
    [ ] Func.locals → Vec<u8> type bytes; zero at activation (matches validator locals)
    [ ] Cuts Module RSS and activation memcpy
[ ] 7. function_type / indirect fast path
    [ ] Cache FuncType by sig index on Func/HostFunc (avoid per-indirect alloc+clone)
[ ] 8. Load metadata cache
    [ ] Record FeatureUsage + export/import summary during decode; feature_usage() reads flag set
    [ ] Optional: inspect-only fast path skipping body decode (future; needs wire scan or cache)

P3 — structural reuse (kernel polish, no face change)
[ ] 9. Split wasm.rs (compile-time modules only)
    [ ] decode.rs · execute.rs · module.rs · store.rs · host_bind.rs
    [ ] Keeps single crate; improves reuse without new products
[ ] 10. Opcode metadata table (validate + execute share stack effect rows)
    [ ] Shrinks triple-match maintenance; may shrink static core with cfg pruning
[ ] 11. Host binding unification
    [ ] Single typed dispatch with compile-time monomorphized bounded buffers
[ ] 12. Legacy Vm quarantine
    [ ] cfg(test) or feature "legacy-vm" default off in release embed builds

Already aligned (keep)
[x] Load-time validation gate before Module handout (validate.rs)
[x] Host Limits ceilings (memory pages, table elems, steps, depth, activation slots)
[x] Bounded decode budget (WASM_MAX_DECODE_ITEMS)
[x] Cooperative interrupt on start instantiation (tip b4883be5)
[x] guest_memory.rs centralizes ptr/len checks
```

### Top 5 ranked cuts (summary)

| Rank | Cut | Primary win |
| ---: | --- | --- |
| **1** | Ephemeral eval `instance_id` / store-slot hygiene | Stops monotonic RSS on `eval_wasm` loops |
| **2** | Slim table templates (no `Vec<Val>` at load) | Halves table-related module memory; removes copy |
| **3** | Shared instantiation bootstrap internal API | One ceiling path; less divergence (start/interrupt) |
| **4** | `Instance::Drop` → unregister store slot | Frees instance state when embed drops handle |
| **5** | `Arc<Module>` + instance overlay | Cuts per-instance duplicate of decoded code/segments |

---

## Explicit non-goals

- **No JS engine growth** — do not expand `tinyvm-qjs` into a full JS runtime; it stays a compile-to-wasm skin.
- **No product merge** — do not fold `agenterm`, game host, or qjs into the wasm kernel crate as competing faces.
- **No second write-cut plan** — this tree is the single lightweight/abstraction backlog; do not open parallel conflicting refactors.
- **No public API widening** — cuts are internal unless explicitly marked optional (e.g. inspect fast path behind existing inspect entry).
- **No JIT / AOT slot** — interpreter-only product sentence unchanged.
- **No implementation in this review** — findings only; land cuts as focused follow-up PRs.
