# Design — the Array milestone for `tinyvm-qjs`

Owner: [tinyvm PRD](../prd/PRD.md) · Downstream consumer:
`agenterm/crates/agenterm-qjswasm`

Status: **stage 1 landed** — the type, the literal, indexing, `length`, and the
property dispatch. Stage 2 is `JSON.parse`/`JSON.stringify` of an array, which
is what the acceptance target in *Why now* actually needs; until it lands,
`fleet.tabs.list()` still comes back as text.

Implementing it corrected this file twice, and both corrections are recorded in
place rather than rewritten away. §2.2's admission rule was **wrong** and is
struck through below. §1.1's promise was **broken by the first implementation**,
measured, and then made true. The two measurements §6 said were owed: one is
now taken and is in §5, the other is still owed and still says so.

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

**Chosen — a second entry point, `__prop_get(receiver_pair, key_pair)` and
`__prop_set(receiver_pair, key_pair, value_pair)`.**

~~Reached only from the Computed arm. A Static key is never an array index — an
IdentifierName is not a canonical numeric string — so the Static arm keeps
today's exact path and pays *nothing*.~~

**That admission rule was wrong, and the first end-to-end run of the milestone
is what said so: `[1,2,3].length` trapped.** Both halves of the sentence are
true and the conclusion does not follow — `a.length` is a **Static key on an
array**, and it does not want to be an index, it wants the header word. Under
the narrower rule it reached `__obj_get`, whose receiver test is
`unbox_object`.

So the gate is the **program**, not the node: when the array set exists, every
member access goes through the dispatcher, and a Static key reaches it as a
boxed String pair. What §1.1 promises is still kept — a program with no array
still pays nothing, byte for byte. What this section additionally *implied*,
that an array-using program's dotted object accesses stay free, was never
load-bearing and is now false by two tag tests and about three bytes per
access (§5).

Still not a per-call-site exemption, which is what §1.2 forbids: the question
asked is a property of the whole program, never a guess about one receiver's
type. The live version of this argument is at `emit::m1::Lower::accessor`.

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

## §5 What it measured

Taken with `compile_qjs_m1`, byte counts of the emitted module, each source
compiled before and after the milestone. The commands are one `cargo run` over
the five sources; the assertions live in
`tests/arrays_m3.rs::a_program_with_no_array_and_no_json_is_byte_identical_to_what_it_was`
and `::naming_json_brings_the_array_set_because_parse_can_return_one`, so the
numbers are locked rather than quoted.

| source | before | after | Δ |
|--------|-------:|------:|--:|
| `return 1;` | 9 784 | 9 784 | **0** |
| `let o = {a:1}; o.b = 2; return o.a;` | 9 905 | 9 905 | **0** |
| `let o = {a:1}; let k = "a"; return o[k];` | 9 865 | 9 865 | **0** |
| `return JSON.stringify({a:1});` | 14 284 | 15 037 | **+753** |
| `scripts/qjs/lib/fleet.js` | 20 935 | 22 076 | **+1 141** |

**§1.1 held only after being broken.** The first implementation cost every
program **11 bytes**, including `return 1;`. The leak was `__typeof`'s and
`__truthy`'s Array arms: `runtime::SET` is unconditional, so an arm appended
there is in every module. Both are now emitted under `runtime::Ctx::arrays`,
the same gate the set uses, and the three array-free rows above are identical
to the byte. Eleven bytes is small and the promise is not — a gate that leaks
is a gate nobody can quote.

**The array set costs 753 bytes**, measured on one source before and after, and
against the JSON set's 4 421 as the comparable.

**`fleet.js`'s +1 141 is 753 + 388.** The remaining 388 is spread across its
~130 member accesses, about three bytes each: the price of the §2.2 correction.

**Still owed, and still not estimated:** the *steps* an indexed loop costs
against the same loop over an object with string keys — the number that either
justifies §2.1's eighth tag or refutes it. §2.1 stands as **reasoned**, not
measured. It is not blocking: nothing in stage 1 depends on the answer, and a
wrong answer would change the representation rather than the surface.

## §6 Acceptance

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

Both measurements this file asked for are accounted for in §5: the byte one is taken,
the step one is still owed and says so.
