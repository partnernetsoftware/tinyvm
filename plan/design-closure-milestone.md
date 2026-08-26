# Design — closures that capture, for `tinyvm-qjs`

Owner: [tinyvm PRD](../prd/PRD.md) · Downstream consumer:
`agenterm/crates/agenterm-qjswasm`

Status: **designed, not implemented.** Written the way
`design-array-milestone.md` was, and for the same reason: reading the call
machinery end to end turned up decisions that look free from outside and are
not. Every number in this file is a byte count or a line of existing code; the
one measurement it owes says so.

## Why this one next

It is the largest single gap left in the language, and it is the only one on
the list that **blocks no gate and blocks everybody**. `agenterm-qjswasm`'s
archive gates for `agenterm-qjs` are all green without it. What it costs is
that this compiles:

```js
function outer() { let a = 1; function inner() { return a; } return inner(); }
```

into a refusal — `this engine does not support closures that capture a
variable yet` (`parse.rs`, the `Res` resolution's last arm). Anything past a
few dozen lines of script reaches for it, so the gap is not felt as a missing
feature but as a ceiling.

Measured, so the gap is the right size rather than the size it feels:

| source | today |
|--------|-------|
| `function o() { let a = 1; function i() { return a; } return i(); }` | **refused** |
| `function mk(n) { return function () { return n; }; }` | **refused** — a parameter is a binding too |
| `let a = 1; function i() { return a; } return i();` | compiles — `Res::Global`, not a capture |
| `function mk() { return function () { return 1; }; } let f = mk(); return f();` | compiles — a function value that captures nothing |
| `function o() { function f(n) { if (n < 2) { return n; } return f(n-1); } return f(3); }` | compiles — `Res::Callee` resolves a function name from any depth |

So what is missing is exactly one thing: **a binding of an enclosing function,
read from inside a nested one** — and a parameter counts, which is the case
every real script hits first.

## §1 Hard constraints

1. **Capture is by binding, not by value.** ECMA-262 closes over the binding,
   so this must answer `2`:
   ```js
   function o() { let a = 1; function i() { return a; } a = 2; return i(); }
   ```
   Assignment is in the subset, so the difference is reachable and a
   by-value capture would be a fabricated answer — the failure this engine
   refuses everywhere else.
2. **A program with no capture pays nothing.** Not "pays little". The array
   milestone made this promise, broke it by 11 bytes, measured it, and fixed
   it; the same bar applies and the same measurement closes it.
3. **No exemption granted at a call site because the compiler knows a type
   there** (`emit.rs::key()`'s rule). A distinction that follows the *grammar*
   or a property of the whole program is allowed; a guess about one callee is
   not.
4. **Detector.** Any urge to make the *uniform* call signature wider "while we
   are here", or to give every function an environment parameter whether or
   not it captures, is the disease this milestone must **detect**. The adapter
   design exists precisely because the cost of a new capability must not land
   on code that never uses it — `emit.rs` rejects two alternatives on exactly
   that ground and names them.

## §2 The five decisions

### 2.1 The environment lives in the function record

`FN_ELEMENT` is one word today and the record's own doc already anticipates
this: *"there are no closures, so there is nothing to capture. Each is one
more word in this record when it lands, and the record is where it goes —
which is the other thing an address buys that an index could not."*

So: `[element: i32][env: i32]`, `FN_BYTES` 4 → 8, and `env` is 0 for a
function that captures nothing.

This is also why the payload had to become an address. The identity fix
(`tests/indirect_attack.rs`) made two evaluations of one function expression
two records; capture is what makes that distinction *observable* —
`function mk(n) { return function () { return n; }; }` must give `mk(1)` and
`mk(2)` two different answers, and they share one element and one adapter.

### 2.2 A captured binding is boxed; only captured ones are

§1.1 forces by-binding capture, so a captured local cannot live in a wasm
local — the frame dies and the closure outlives it. It moves to a one-word
heap cell, and **both** the declaring function and the closure read and write
through that cell. The declaring function reading its own local directly while
the closure reads the cell is the classic bug: they diverge on the first
assignment.

**Only bindings that are actually captured are boxed.** That needs a
resolution answer the parser does not produce today: `Res::Local` says "a
binding of the function this occurrence is in" and nothing says "…and someone
inner reads it". A pass over the resolved tree that marks captured bindings is
the smallest addition; it runs after resolution, when every occurrence's
`Res` is known.

Boxing every local instead would be simpler and is refused under §1.2: a
script with no closure would pay an allocation per local per call.

### 2.3 The environment is passed, not baked in

This is the decision that looks free and is not.

The table holds **one adapter per function that became a value** — per
*function*, not per closure instance. Two closures over different environments
share one element and one adapter, so the environment cannot be part of the
adapter's identity. It has to arrive as data.

Three places it could arrive from:

* **Baked into the element.** Rejected by the sentence above: it would need one
  table element per closure *instance*, and instances are created at run time.
* **Read by the callee from "the current function".** There is no such thing;
  wasm has no notion of the callee's own funcref.
* **Passed as an argument.** Chosen. A capturing function takes one leading
  `i32` parameter, its environment pointer. A **direct** call passes it
  statically — the caller is the enclosing function and knows its own cell
  vector. An **indirect** call goes through the adapter, and the call site
  already holds the function value whose payload *is* the record, so it loads
  `env` from the record and pushes it.

### 2.4 The uniform signature gains one leading slot, under a gate

The adapter's signature is uniform across the table, so if any adapter
forwards an environment, all of them take the slot. That is one `i32` pushed
per indirect call site, in a program that has closures.

Under §1.2 and §1.4 it must be gated: a program where no function captures
emits the uniform signature it emits today, unchanged, and the exact predicate
is **some function captures a binding of an enclosing function** — which the
pass in §2.2 already computes. It is exact for the same reason the array
gate's is: it is a property of the resolved tree, not a guess about syntax.

A non-capturing target reached through a widened adapter simply drops the
slot, the same way the adapter already drops surplus arguments (13.3.8.1).

### 2.5 The cell vector is its own record, not an Array

`[n: i32][cell: i32]…`, allocated when a function value that captures is
created. Deliberately **not** the `TAG_ARRAY` record: an environment is not a
JavaScript value, nothing can reach it from the language, and giving it a tag
would put it one `typeof` away from being one. The array record also carries a
capacity word and stores whole V1 pairs, neither of which an environment
needs.

## §3 What this does not answer

- **`var` hoisting into a closure**, and the TDZ interaction with a captured
  `let`. The subset has both keywords with a textually-decidable TDZ; whether
  a captured binding's TDZ is still decidable is an open question this
  milestone must answer before it lands, not after.
- **Recursive capture** — a closure that captures itself. Half of this is
  already answered: a nested function *calling itself by name* compiles today,
  because `Res::Callee` names the function rather than storage and resolves
  from any depth (measured, in the table above). What is untested is a nested
  function *reading its own name as a value* from inside a capturing scope.
- **Arrow functions**, which are a separate parser milestone and would share
  this machinery.
- **Escaping loop variables** — `for (let i …)` creating one binding per
  iteration (ECMA-262 14.7.4.7). This is the case every engine gets wrong
  first, and the subset has `let` in a `for` head today.
- **GC.** A cell keeps a value alive as long as the closure does; the heap is
  bump-allocated and never freed, so this changes nothing yet and everything
  when GC lands.

## §4 Acceptance

Not "closures exist". Four things, each falsifiable:

1. Capture is by binding: the §1.1 program answers `2`, not `1`.
2. Two closures over one function expression have separate environments:
   `mk(1)` and `mk(2)` answer `1` and `2`.
3. A program with **no** capture is byte-identical to what it emits today —
   the §1.2 promise, measured the way the array gate measured it.
4. `agenterm/scripts/qjs/lib/fleet.qjs` still compiles to the same bytes,
   because it captures nothing and must therefore pay nothing.

**One measurement is owed and may not be estimated**: the bytes a program with
one closure pays over the same program without one, split into the fixed part
(the widened uniform signature) and the per-closure part (the record word, the
cell vector, the boxing). §2.4's gate stands as *reasoned* until that number
exists.
