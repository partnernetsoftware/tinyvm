# Design — the Array milestone for `tinyvm-qjs`

Owner: [tinyvm PRD](../prd/PRD.md) · Downstream consumer:
`agenterm/crates/agenterm-qjswasm`

Status: **designed, not implemented.** Written after reading the whole Object
path end to end, because four decisions a straightforward implementation gets
wrong were only visible from there. Nothing in this file is measured; every
number in it is a byte count of existing code, and the two places where a
measurement is owed say so.

## Why now

`crates/tinyvm-qjs/README.md` already states the consequence at the boundary
it belongs to:

```js
JSON.parse("[1]")   // throws: this engine does not support JSON arrays yet
```

> The Array one has a downstream consequence worth stating: `fleet.js` wraps
> every broker answer in `try { JSON.parse(t) } catch { return t }`, so an
> answer that is or contains a JSON array — `tabs.list` is the obvious one —
> comes back as the raw text and a caller expecting a value gets a String.

Downstream that is now pinned by a test rather than by a sentence:
`agenterm/tests/script_engine_equivalence.rs` runs the same script on
`agenterm-qjs` (rquickjs) and `agenterm-qjswasm` (this engine) through both
shipped fleet bindings and requires them to agree. Four cases agree. The fifth
is `tabs.list`, and it is written as a **named divergence**: rquickjs yields an
array, this engine yields the raw text. It is the only difference left between
the two engines, and when arrays land that test fails and is meant to be moved
in with the other four.

So the milestone's acceptance target is not "arrays exist". It is:
**`fleet.tabs.list()` returns something a script can index.**

## §1 Hard constraints

1. **A program with no array pays nothing.** Not "pays little" — nothing: no
   function, no element, no global, no byte. This is the bar `JSON` already
   meets (`README.md`: "Nothing at all for a program that does not name it")
   and the bar the measured growth law in `RESULTS.md` exists to enforce.
2. **No exemption granted at a call site because the compiler knows a type
   there.** `emit.rs`'s `key()` states this rule about itself and it applies
   here. A distinction that follows the *grammar* — Static key vs Computed key
   — is a property of the node and is allowed. A distinction that follows a
   guess about the receiver is not.
3. **Dispatch order is Number, then String, then everything else** (`repr.rs`
   module header). A new type is added by **appending** an arm. Where a site
   departs, it says why at the site.
4. **Detector.** Any urge to give the array record a second, general property
   store "so `a.foo = 1` works" is the disease this milestone must *detect*,
   not satisfy. Record it as a finding; do not quietly add it.

## §2 The four decisions

### 2.1 Array is an eighth tag, not an Object with integer keys

`TAG_ARRAY = 7`, payload a guest pointer, appended for the reason
`TAG_OBJECT = 5` and `TAG_FUNCTION = 6` were.

ECMA-262 10.4.2 makes an Array an exotic *Object*, and the existing record
could already express one: keys are Strings and `o[1]` and `o["1"]` are one
slot (7.1.19), so `a[0]` would work today if an array were just an object.
It is rejected on cost, and the cost is not marginal. `obj_find` walks the
entries calling `__str_eq`, and the key for `a[i]` is a **Number**, so every
index access would run `__num_to_string` — Dragon4, the shortest
round-tripping 6.1.6.1.20 — to build a fresh record, then scan. A dense vector
reads element `i` with one bounds test and one multiply-add.

An array's whole reason to exist is that the index *is* the address.

Layout, mirroring `OBJ_HEADER` exactly so the growth code is the same shape:

```text
ARR_HEADER = 12       [len: i32][cap: i32][elems: i32]
ELEM_BYTES = 12       [tag: i32][payload: i64]     -- the V1 pair stored whole
```

The pair is stored whole, tag beside payload, for the reason
`ENTRY_TAG`/`ENTRY_PAYLOAD` give: a read is two loads and not a re-boxing.
`i64` at offset 4 under `ALIGN_WORD` is below-natural alignment, which is legal
wasm and a hint only — the object record already does this at `ENTRY_PAYLOAD`.

### 2.2 The index fast path lives behind the *grammar*, not behind a guess

This is the decision most likely to be got wrong, and it is worth being
precise about why the obvious two answers both fail.

Today a member access emits, in `emit.rs::key()`:

- **Static** key (`o.a`): intern the name, push the pointer.
- **Computed** key (`o[k]`): evaluate, then `__to_string` (which is also
  7.1.19 ToPropertyKey).

and then calls `__obj_get(receiver_pair, key_ptr)`.

**Rejected — widen `__obj_get` to take the key as a pair.** It would let the
Array arm see a Number before it is stringified, but it puts one extra tag
test in front of *every* property read, including the dozens of static reads
`fleet.qjs` performs. That is a cost paid by every program that has no array,
which §1.1 forbids.

**Rejected — recognise an array receiver at the emit site.** The compiler
usually cannot know, and where it can, §1.2 forbids acting on it.

**Chosen — a second entry point reached only from the Computed arm.**
`__prop_get(receiver_pair, key_pair)` and `__prop_set(receiver_pair,
key_pair, value_pair)`. A Static key is never an array index — an
IdentifierName is not a canonical numeric string — so the Static arm keeps
today's exact path and pays *nothing*. The Computed arm pays one dispatch,
which it needs regardless.

This is legal under §1.2 for the reason `emit.rs::key()` already writes down
about itself: the distinction is Static-vs-Computed, a property of the node,
which no later change can quietly widen.

`__prop_get`, in order:

```text
receiver is Object   -> __obj_get(recv, __to_string(key))     -- unchanged path
receiver is Array
    key is Number, integral, in [0, len)  -> element load
    key is Number otherwise               -> undefined
    key is anything else                  -> k = __to_string(key)
                                             k == "length" -> Number(len)
                                             otherwise     -> undefined
otherwise            -> trap                                  -- undefined[k]
```

Object is tested first, the departure `obj_get` already documents and for the
same reason: in every non-erroneous program the receiver of a property access
is an Object, so testing anything before it puts a test in front of the only
path that ever succeeds.

### 2.3 There are no holes, and that is a statement about what is observable

ECMA-262 makes `a[5] = 1` on a length-2 array produce length 6 with four
*holes*, which are distinguishable from `undefined` only through `in`,
`hasOwnProperty`, `Object.keys`, and the iteration methods that skip them.
This engine has none of those, and will not have them in this milestone.

So a set past the end **fills with `undefined`** and the difference is not
observable from any script this engine can run. This is written down rather
than left implicit because it stops being true the moment `in` or
`Array.prototype.forEach` arrives, and whoever adds one of those needs to find
this paragraph rather than a surprise.

### 2.4 A non-index property on an array is refused by name, not faked

`a.foo` reads `undefined`, which is correct and free — 10.1.8.1 with no
prototype. `a.foo = 1` has nowhere to go: the record is a dense vector with no
key space. It is refused with a named capability diagnostic, in the engine's
existing voice, rather than being silently dropped or triggering §1.4's
disease.

`a.length = 0` is the same shape and gets its own sentence, because
truncation is a real thing scripts do and "not supported yet" is a more useful
answer than a generic property refusal.

## §3 Gating: the part that is easy to get wrong

`runtime::SET` is **unconditional** — `Rt::offset()` is a position in that
slice and `func_base + offset` is a fixed layout, so every module emits all 28
of those functions. Adding six array functions there would charge every
array-free program for them, which §1.1 forbids.

The precedent to follow is `convert::JSON_SET`, which is emitted after the
runtime block at a computed `json_base` and only when the predicate holds.

**The predicate must be exact**, and the README says why: `JSON`'s gate is the
*name appearing*, "because that predicate is exact where 'contains an
addition' is not". The exact predicate here is:

> the program contains an ArrayLiteral node, **or** the program names `JSON`.

`JSON` is in it because `JSON.parse` can produce an array from text the
compiler never sees. No other construct can bring an array into existence:
`o[k]` on a program with no array literal and no `JSON` can never find one,
so the Computed arm keeps calling `__obj_get` directly and the `__prop_*` pair
is not emitted at all.

## §4 What this does not answer

Each of these is a candidate for its own milestone, and none is required by
the acceptance target in *Why now*.

- **Array methods.** `push`, `map`, `join`, `indexOf`. They need
  `Array.prototype`, and this engine has no prototypes; `JSON` is bound as an
  ordinary object holding function values precisely to avoid needing one.
- **`Array.isArray`, the `Array` constructor, `new`.**
- **`for…of` and destructuring over arrays.** Both are refused at the parser
  today and neither becomes reachable by adding the type.
- **An Array crossing the host boundary.** `Value` has no Object variant and
  will not gain an Array one here: the same argument applies (a guest heap
  reference the host has no layout for and no way to keep alive). A script
  that wants the host to see an array's contents returns a property or a
  `JSON.stringify` of it.
- **Sparse-array memory behaviour.** `a[1000000] = 1` allocates a million
  elements under §2.3. That is 12 MB and will hit `max_memory_pages`, which is
  a budget refusal and the honest outcome — but it is a denial-of-service
  shape worth an adversarial test in `heap_attack.rs` rather than a note.

## §5 Acceptance

Not "arrays exist". Three things, in order, each falsifiable:

1. `JSON.parse("[1,2,3]")` returns an Array, and `JSON.stringify` of one
   round-trips — including an array nested in an object, which is what a
   broker answer actually looks like.
2. `fleet.tabs.list()` through the real `agenterm/scripts/qjs/lib/fleet.qjs`
   returns something a script can index and take `.length` of.
3. `agenterm/tests/script_engine_equivalence.rs`'s fifth case fails, and is
   moved in with the other four — the two engines then agree on all five.

Item 3 is the one that matters: it is the only case in that file written to
fail on success, and it is what turns "arrays landed" from a claim into a
measurement someone else can rerun.

**Two measurements are owed and are not in this file**: the bytes the gated
array set adds to a module that uses one (against the `JSON` set's 4 421 as
the comparable), and the steps a `for` loop over `a[i]` costs against the same
loop over an object with the same number of properties — the number that
either justifies §2.1 or refutes it. Neither may be estimated; §2.1 stands as
*reasoned* until they exist.
