# tinyvm-qjs

The `.qjs` → `.wasm` compiler, in pure Rust, plus the language skin over
[`eval_wasm(data, globals, locals)`](../tinyvm). Not rquickjs, not a QuickJS C
binding, and not a full JS engine.

```text
source  --lex-->  tokens  --parse-->  AST  --emit-->  wasm IR  --encode-->  bytes
```

Five stages, five modules, plus two the value representation adds: `repr` is the
shape of a JavaScript value and `runtime` is the guest-side code every compiled
module carries, because an operator that dispatches on its operands' types is a
call and not an opcode.

The encoder is hand-written on purpose: the output has to clear tinyvm's load
gate, which is strict about canonical section order, minimal LEB128 and exact
expression termination, so this crate owns that correctness instead of assuming
it from a dependency. `wat` is a dev-dependency only, and only so the suite can
compare this encoder's bytes against a reference assembler's rather than assert
they are canonical.

## Two entry points

`compile_qjs_m1` is the language. `compile_qjs` is the older `i32`-in/`i32`-out
expression compiler, kept only until its callers move; when they do, M1 takes
the name.

```rust
use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

let wasm = compile_qjs_m1("return $0 * 2;")?;
// `WasmError` derives `Debug` but is not a `std::error::Error` -- the core is
// `no_std` -- so `expect` rather than `?`.
let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the gate");
let mut instance = module.instantiate().expect("instantiates");
let out = instance
    .invoke_by_name("main", &Value::args(&[Value::Number(21.0)]))
    .expect("runs");
assert_eq!(Value::returned(&out), Ok(Value::Number(42.0)));
```

One JavaScript value is **two** wasm values — a `(tag: i32, payload: i64)` pair
— so `main` takes two parameters per argument and returns two results. `Value`
is the door across that boundary; `Value::String` is a pointer into the
instance's linear memory, not text, and resolving it needs the instance.

## What it compiles

- **Numbers**: binary64 throughout. `1/10 + 2/10 !== 3/10`, `2147483647 + 1`
  does not wrap, `1/0` is `Infinity`, `0/0` is `NaN`, and `-0` is distinct from
  `0`. Literals are still written as decimal integers in the `i32` range —
  `1.5`, `1e3`, `0x10`, `1_000` and `0777` each name their own boundary.
- **Other values**: strings (escapes decoded, `\u{…}` and surrogate pairs
  included), `true`/`false`, `null`, `undefined`.
- **Statements**: `let`/`const`/`var` with real scoping and a temporal dead zone
  the text can settle, blocks, `if`/`else`, `while`, three-part `for`, `return`,
  `throw` and `try`/`catch`/`finally`, and the script's ECMA-262 completion
  value. A finalizer runs on all three of its paths — fall-through, `return`
  and a throw — an abrupt one replaces what was pending (`try { return 1; }
  finally { return 2; }` is `2`), and a normal one contributes **no value at
  all**, which is 14.15.3 step 3 and is why `try { 1; } finally { 2; }` is `1`.
  See "A throw, where the machine has no exceptions" below.
- **Functions**: declarations and expressions, named or not, with parameters,
  recursion and mutual recursion — and a function is a **value**. It can be
  stored in a binding or a property, passed, returned, and called from
  wherever it ended up: `o.m()`, `o.a.b()`, `f()()`. `typeof` answers
  `"function"`, every one of them is truthy, and `===` on two of them is
  identity — where *identity* means ECMA-262 15.2.5's: each **evaluation** of
  a function expression is a new object, so `mk() === mk()` is `false` and
  reading one binding twice is `true`. A call with too few arguments passes `undefined` and one with too
  many evaluates and discards the surplus. Calling something that is *not* a
  function **traps** — ECMA-262 makes it a TypeError and there is no `throw`
  here — and it traps at the tag test, before any table is reached. Two
  things a function value still is not: it has no `this` (so `o.m()` calls the
  function `o.m` holds and the function cannot see `o`), and it has no
  prototype (so `f.call`, `f.bind` and `f.length` are a trap and not a
  method).
- **Operators**: every rung the ladder has — assignment and its compound forms,
  the conditional `? :`, `??`, `||`, `&&`, `==`/`!=`/`===`/`!==`, `<` `<=` `>` `>=`,
  `+` `-`, `*` `/` `%`, prefix and postfix `++`/`--`, unary `+ - !`, and
  grouping. `?:` is right-associative and only the taken branch evaluates
  (13.14), which is checked by an observable side effect rather than by reading
  the emitted code. `??` evaluates its right operand only when the left is
  `null` or `undefined`; `false`, `0`, and the empty string are retained. This
  first slice refuses every combination of `??` with `&&` or `||` by name,
  including parenthesized combinations, and does not include `??=`. `&&` and
  `||` short-circuit; `+` concatenates when **either** side is a String, running
  ToString on both — ECMA-262 13.15.3 step 1.d, and see "The three
  conversions" below. `%` is
  ECMA-262's remainder, with the sign of the dividend and exact for operands a
  rounded quotient would get wrong — `-6 % 3` is `-0` and
  `2147483647 * 2147483647 % 1000` is `608`. `typeof` answers with the
  ECMA-262 13.5.3 name of each of the five types this engine has, `typeof null
  === "object"` included; a name the source never declares — anything but
  `JSON`, the one name this engine binds — is still refused before `typeof`
  sees it, because there is no global scope for it to be absent from.
- **Objects**: literals (`{}`, `{ a: 1 }`, shorthand `{ a }`, a trailing comma,
  and string- or number-literal keys), property reads by dot and by computed
  key, the first optional-chain slice (`base?.prop` / `base?.[key]`), and
  property assignment including the compound and update forms —
  `o.a += 2`, `o.a++`. Keys are Strings, so `o[1]` and `o["1"]` are one slot
  and `o[0.5]` is the property `"0.5"`;
  a property that is not there reads `undefined` rather than trapping;
  optional access evaluates its base once and does not evaluate a computed key
  when that base is `null` or `undefined`. Optional calls and continuation
  chains such as `o?.a.b` remain named capability boundaries rather than being
  approximated with the wrong short-circuit extent;
  property order is insertion order; and `===` on two Objects is reference
  identity. Reading a property *of* a primitive (`"abc".length`, `(1).a`)
  traps: there is no prototype here, and answering `undefined` would be a right
  answer by a wrong route for exactly the members a script reaches for.
- **`JSON`**: `JSON.parse` and `JSON.stringify`, ECMA-262 25.5, whole
  algorithms rather than the easy part of them — see "`JSON` is an object, not
  an intrinsic" below. It is the **one** name this engine binds itself, and a
  script that declares its own `JSON` shadows it outright.

- **ASI**: ECMA-262 12.10, split where the spec splits it. Rule 3 is a fact about
  the token stream and lives in the lexer; rules 1 and 2 need a parser and live
  in the parser; the `for`-header override lives where the grammar position is.

## The object record, and why it is a vector

`runtime.rs`, `OBJ_HEADER`: `[len: i32][cap: i32][entries: i32]`, over entries
of `[key: i32][tag: i32][payload: i64]`. A flat key/value vector, scanned
linearly — not a shape table.

That is a decision about a population, not a general claim. The library this
milestone exists to compile builds twelve namespace tables of twelve *different*
shapes with one instance each, and one- to three-field parameter objects made
fresh per call and read once. A hidden class pays for itself by sharing a shape
across many objects; neither of those populations shares one, so a shape table
would be twelve entries used once apiece plus a second allocation per object, to
remove a scan of one to three key comparisons. The three conditions that would
overturn it — many objects of one shape, records past ~16 properties, a key
looked up in a loop — are written at the constant, with the note that entry
indices are stable under growth, so an inline cache is already possible.

Objects are `TAG_OBJECT`, a sixth tag on the same `(tag, payload)` pair. The
value-representation experiment measured the growth law this now pays: each
extra type costs one type test per dispatch site. So every Object arm is
appended **last** in `__typeof`, `__truthy` and `__to_number` — no type that
existed before objects pays for them — and the two sites that depart from
Number-first say so where they depart: `__obj_get`/`__obj_set` test Object first
(in a non-erroneous program the receiver *is* one), `__to_key` tests String
first. `===` on two Objects needed no new arm at all: the payload comparison
already is reference identity, which is ECMA-262 7.2.15 step 4.

Functions are `TAG_FUNCTION`, a seventh tag, and they paid the law's price
exactly: one arm appended last in each of those same three functions, and
nothing anywhere else. `===` again needed no arm, and *why* it needed none is
worth reading, because the first answer was wrong. It said "one function gets
one element index however many times it is read", which is true and is not a
proof: the case it does not cover is one function expression **evaluated**
twice, which ECMA-262 15.2.5 makes two objects and an element index made one.
So a function value's payload is the address of a per-evaluation record, and
the arm is right for exactly the reason the Object arm is: a bump allocator
hands out one address per object. The site a function value runs hot, the
call, is not a ladder at all but a single tag test, which is one test whatever
the tag domain grows to.

## A throw, where the machine has no exceptions

tinyvm has no wasm exception handling: `crates/tinyvm/src/wasm.rs` has no arm
for `try` (0x06), `catch` (0x07), `throw` (0x08), `rethrow` (0x09) or
`try_table` (0x1F), and its section ranking covers ids 1..=12, so the tag
section is refused at the gate. The PRD's capability tree says
`exception handling [ ]` and this did not change that.

So a `throw` is **a flag, and a check after every call that could raise one**.
Three module globals hold a throw in flight — the flag, and the thrown value's
`(tag, payload)` pair, because ECMA-262 lets any value be thrown and this
engine keeps that. Three and not one: the flag cannot fold into the tag,
because `TAG_UNDEFINED` is `0` and `throw undefined` is a real program.

The two designs that lost are written at `emit::m1`'s `Unwind`. A **sentinel
value the caller tests** would be an eighth tag, which the value-representation
experiment's measured growth law prices at one more type test at every dispatch
site, paid by every program whether or not it throws — and a completion record
is not a language value. A **table of handler continuations** needs a computed
jump, so every function body becomes `loop` + `br_table`: it rewrites the
non-throwing path to buy the throwing one.

What the chosen one costs, on the path where nothing throws:

| | |
| --- | --- |
| a program with no `throw` and no `JSON` | **nothing** — not one instruction, not one global |
| per direct call site | 2 instructions / **4 bytes** |
| per `call_indirect` site | 2 instructions / **4 bytes** |

The per-call-site number is a *second difference* over two programs identical
but for whether one function throws, so nothing else in the module has to hold
still (`a_throwing_program_pays_four_bytes_per_call_site` in
`tests/conditional_and_try.rs`); the zero is checked structurally, by counting
the emitted global section
(`a_program_that_cannot_throw_declares_no_unwinding_global`). The `br_if`'s
target is the nearest enclosing handler, or — where there is none — the
function's own label, which *is* a return, and the pair already on the stack is
the callee's, so nothing has to be built to satisfy the return arity. That is
what keeps it at two instructions.

Three things this shape is not, and each is a real divergence rather than an
oversight:

- **A trap is not a throw.** ECMA-262 makes `undefined.a` a TypeError and a
  `catch` takes it; here it is an `unreachable` and the clause never runs.
  There are no `Error` objects and no prototype to hang one on, so a `catch`
  that swallowed a fault would have no value to describe it.
- **The channel belongs to one call.** The globals are instance state, and an
  uncaught throw traps with the flag raised, so the entry prologue clears it
  the same way it clears the fault word. Without that, one uncaught throw
  poisoned a persistent instance for its lifetime: the next call's `catch`
  fired with no `throw` on any path it took, bound to the previous call's
  value — a pointer into the previous call's heap where that value was an
  Object. `tests/unwind_attack.rs` is where it was found and where it is held.
- **The thrown value does not reach the host.** An uncaught throw reports
  itself as `GuestFault::UncaughtThrow` (below) and nothing more. Handing the
  value out would mean exporting an engine-internal pair or widening the entry
  point's results, and both are decisions about the host boundary rather than
  about throwing.

## `JSON` is an object, not an intrinsic

`JSON.parse` and `JSON.stringify`, ECMA-262 25.5. Twenty-two emitted functions
in a **gated** set: `convert::SET` is unconditional because "does this program
contain an addition" is an over-approximation, and "does this program name
`JSON`" is exact, so a program that never writes the name is byte-identical to
what it was.

`__json_ns` calls `__obj_new`, `__fn_new` twice and `__obj_set` twice — the
same three runtime functions a script writing
`const JSON = { stringify: function () {}, parse: function () {} }` reaches.
Reading `JSON.parse` is `__obj_get`; calling it is `call_indirect` through an
adapter in the module's own funcref table, on the one uniform signature every
call through a value speaks. `typeof JSON` is `"object"`,
`typeof JSON.parse` is `"function"`, and `JSON === JSON` is `true` because the
object is built once per instance and read out of a global pair.

The name is bound by `ast::Res::Json`, and it is **one name and not a global
scope**. Resolution walks the scopes first, so a script's own
`const JSON = {…}` shadows it outright and there is nothing privileged to lose
to; an embedder that *declares* a host function called `JSON` wins too, because
a declaration table is an explicit act where `Names::HostImport`'s "any free
name is an import" is a default. Nothing enumerates it, and there is no
environment record. A second intrinsic would make this a table, and that is the
point at which "no global scope" would stop being true.

Three places the specification beat the obvious, each with a test:

- **U+2028 and U+2029 are not escaped.** 25.5.2.2 QuoteJSONString escapes its
  seven table characters, everything below U+0020, and lone surrogates — and
  nothing else. Escaping the line separators is a habit from embedding JSON in
  JavaScript *source*, which is a different problem.
- **`JSON.stringify(-0)` is `"0"`** (6.1.6.1.20 step 2), so a round trip loses
  the sign at the printer, exactly where the spec loses it. The *parsed* value
  is still negative zero.
- **`1e400` is `Infinity`, and then `null`.** `serde_json` rejects the text;
  the spec rounds.

A growable buffer rather than `__str_concat`, because quoting appends one to
six bytes per source byte and a concatenation per byte would be quadratic in
allocation on a heap that never frees. Cycles are checked against a chain of
*ancestors* and not a seen-set, so a DAG still serializes twice, as 25.5.2.2
requires.

One boundary is the engine's and says so:

```js
JSON.stringify(o, null, 2)        // throws: no replacer and no space argument yet
```

There used to be a second — `JSON.parse("[1]")` threw "this engine does not
support JSON arrays yet" — with a downstream consequence worth stating:
`fleet.js` wraps every broker answer in `try { JSON.parse(t) } catch { return
t }`, so an answer that was or contained a JSON array (`tabs.list` being the
obvious one) came back as raw text and a caller expecting a value got a String.

**The Array milestone closed it.** `JSON.parse("[1,2,3]")` is an Array,
`JSON.stringify` of one round-trips, and an array nested in an object works in
both directions. Two spec details are worth naming because they are easy to
get backwards: 25.5.2.5 step 8 writes `null` for an element that serializes to
nothing, where 25.5.2.4 step 5 *omits* the property — so `[undefined,1]` is
`[null,1]` while `{a:undefined,b:1}` is `{"b":1}`; and an array that contains
itself is the same catchable TypeError an object that does gets, walking the
same ancestor chain, so a DAG still serializes twice.

Naming `JSON` also turns the unwind channel on, whether or not the script
writes `throw`, because `JSON.parse` raises one. That is the condition
`convert::JsonCtx::unwind` states and `emit::m1::scan` satisfies, and without
it `fleet.js` lines 15-19 would trap where the library wrote a `catch`.

**What it costs**: 4 421 bytes for the set and the channel together, measured
as one mention of the name added to `return 1;` (9 784 to 14 205). Nothing at
all for a program that does not name it: no function, no adapter, no element,
no global, no byte.

Nothing here forced an intrinsic, and the urge was real: `JSON.stringify(o)` is
a statically known callee at all nine sites in `fleet.js`, so a direct call
would save a property read, a tag test and a `call_indirect` each time. It was
refused on `repr.rs`'s own grounds — an exemption written into one call site is
one the compiler has no pass to check, and `const f = JSON.stringify` would
have to agree with it. The cure, when measured and wanted, is a general
devirtualisation pass over a property read of a known-constant object.

## The three conversions, and the fourth that is still missing

`+` concatenates whenever **either** side is a String, `-`/`*`/`/`/`%` and the
unary operators convert a numeric String, `<` and its three siblings compare
two Strings by code unit, and `==` bridges a Number and a String:

```js
"a" + 1     // "a1"
1 + "a"     // "1a"
"a" + true  // "atrue", and so do null and undefined
"1" - 1     // 0
-"  42  "   // -42, and +"0x1f" is 31, and +"" is 0, and +"nope" is NaN
"a" < "b"   // true
1 == "1"    // true   (`===` answers false, correctly)
```

Three ECMA-262 algorithms sit behind that, in `convert.rs`, and each is the
whole algorithm rather than the easy part of it:

- **Number::toString (6.1.6.1.20)** is Steele & White's free-format Dragon4 in
  Burger & Dybvig's formulation: the value is held as an exact rational with
  the half-gaps to its two neighbours, and digits come out until one
  distinguishes it from both. Step 5's three conditions all bite — *shortest*
  is why `0.1` prints `0.1`, *closest* is why `0.1 + 0.2` prints
  `0.30000000000000004`, and *even* is why `785068460487425.25` prints
  `…5.2` where Rust's own shortest-round-trip formatter prints `…5.3`. Steps 6
  to 9 place the point, and they are the spec's thresholds and not a
  formatter's: exponential above `n > 21` and below `n <= -6`.
- **StringToNumber (7.1.4.1)** is the whole `StrNumericLiteral` grammar —
  whitespace, sign, `Infinity`, hex/octal/binary, and the empty string being
  `+0`. Its core is exact: the decimal becomes a ratio of big integers and is
  divided **once**, so the answer is correctly rounded and not an accumulation
  of roundings. The one fast path is Clinger 1990's theorem and not a guess.
- **String relational comparison (7.2.13)** decodes to UTF-16 code units. A
  byte compare would be wrong: UTF-8 byte order is *code point* order, so it
  answers `"\u{10000}" > "\u{E000}"` and the spec answers the other way.

There is a bignum behind the first two, in 16-bit limbs, and the limb size is
chosen by the instruction set rather than by preference: `repr.rs`'s `Ins` has
no `i32.shr_u` and no 64-bit arithmetic, so carry extraction is `i32.div_s`,
which is unsigned only below `2^31`.

**They cost 6 625 bytes in every emitted module**, measured through this
crate's own encoder by stubbing the 23 conversion bodies: an empty script went
2 620 to 9 771. That is unconditional, like the rest of the runtime prelude,
and it is the largest single thing a compiled module carries. The lever, if
the number ever matters, is per-algorithm gating —
`the_emitted_size_of_each_conversion_is_written_down` in `tests/conversions.rs`
prints what each one weighs — and the reason it is not pulled now is that the
predicate is "does this program contain an addition or a comparison", whose
false negative is a trap where an answer was due.

The conversion that is **still** missing is a fourth one, ToPrimitive (7.1.1).
It needs the `valueOf`/`toString` a prototype would carry, and there is no
prototype, so an Object or a function operand traps:

```js
"" + {}     // traps; JavaScript says "[object Object]"
{} + ""     // traps
const o = {}, k = {}; o[k]   // traps; a key runs ToString too
```

ToBoolean needs no prototype, so the truthiness ladder answers, and every
Object and every function is truthy.

## The function value, and why there is a table

wasm MVP has no first-class function reference, so a call through a value is
`call_indirect` on the module's own funcref table. That instruction matches the
callee's signature *exactly* (spec 4.4.8), and JavaScript's calls match
nothing, so the table does not hold the user's functions: it holds one
**adapter** per function that became a value, all of one uniform signature,
each forwarding as many arguments as its target declares and letting the rest
fall away. The uniform arity is a bound — the widest parameter list in the
program, or the widest indirect call site, whichever is more.

The alternative designs and why they lost are written at `emit::m1`'s header.
The short form: adapting *at the call site* would put one `call_indirect` per
arity at every site, and adapting *in the callee*, by giving every function one
wide signature, would make a single eight-parameter function anywhere charge
every zero-argument direct call eight `undefined` pairs. The adapter keeps the
cost on the functions that became values.

`TAG_FUNCTION`'s payload is **a guest pointer to a one-word record holding the
element index**, and not the index itself. The index was the first answer and
it was wrong: one function expression has one index however many times it is
*evaluated*, and ECMA-262 15.2.5 makes each evaluation a new object, so
`function mk() { return function () {}; } mk() === mk()` answered `true`. One
address per evaluation is what makes them two, and the address is what the
allocator already hands out — so `===` stayed a payload comparison with no arm
added, for exactly the reason it works for Object. A function value is
therefore something a *scope* holds: a declaration is instantiated when the
statement list holding it is entered, `const f = function () {}` at its
declarator, and a binding whose name is never read as a value costs nothing at
all.

Three guards stand between a payload and a jump, and they are in this order:
the tag test, which is what makes `undefined()` a clean fault; a range check
that the payload *fits* the pointer it is about to become, because narrowing
an `i64` with a bare `i32.wrap_i64` let `(TAG_FUNCTION, 2^32 + 1)` reach
element 1 silently; and element 0 being left null, so a zeroed word is never a
callable element. `tests/indirect_attack.rs` is where all three are attacked.

A script that makes no function a value and never names `JSON` emits **no
table, no element segment and no adapter**; naming `JSON` puts two adapters of
its own in that table, before the script's, because their element indices are
needed by the entry prologue and the count of the script's is not known until
lowering has finished. What every script pays is the growth law's price of a seventh
type — one arm appended last in each of `__typeof`, `__truthy` and
`__to_number` — plus 29 bytes for `__fn_new`. Measured on this crate's own
encoder: a function-valued property costs about **128 bytes** (the function,
its adapter, its element and the assignment), a call through a value costs
about **70 bytes** where the direct call it replaces costs about 28, and
`fleet.js` in whole comes to 22 457 — it was 20 935 before the Array
milestone, which added 1 130 for the array set and 392 spread across the
library's ~130 member accesses.

## What it does not compile, and how it says so

`this`, `class`, `for…in`, `switch`, tagged templates, the comma operator,
`**`, `??`, BigInt, numeric separators, and array elisions remain outside the
subset.

`?:`, `throw`/`try` and `JSON` came off that list, and one consequence is
worth stating because it changes what a diagnostic says: `Object.keys(o)` stops
at the *name*. The engine can make the call; it has no binding named `Object`
to make it on, and `JSON` is the only name it binds itself. `o.toString()`
compiles and traps instead, because the property is simply absent.

Two object-shaped refusals are their own sentences: a property named with a
reserved word the lexer spells as a keyword (`o.new`, `o.class`, `o.default` —
ECMA-262 13.2.5 makes every IdentifierName a legal PropertyName, so this is a
gap in the parser and not in the language), and `__proto__` as a literal key,
which 13.2.5.5 makes a prototype assignment rather than an own property and
which is refused rather than silently made into the wrong property.

Each rejection is a `CompileError` whose sentence names the *engine's* boundary
— "this engine does not support X yet" — never "syntax error", and carries the
byte offset where the construct starts.

That wording is a product requirement, not a style preference. A subset this
small rejects mostly perfectly good scripts, so a sentence blaming the author
would be a lie, and a sentence that names no boundary leaves the reader guessing
where the engine stops.

Which is why the *other* direction is a lie too, and it took a conformance
corpus to notice. `o.m = function (x) { return x; } function rec() {}` — two
statements, one line, no `;` — used to be refused with "this engine does not
support the `function` keyword yet", a keyword the engine has had since M1.
That sends the reader hunting for a workaround instead of at the missing
semicolon. `else { }` said the same about `else`, next to a suite full of
working `else` arms, and the milestone that landed `?:` and `throw` added two
more of exactly that shape — turning a feature on does not turn its
"unsupported" sentence off.

The rule is now one rule, at `Parser::cannot_use`. The lexer's capability table
is shared with M0's expression compiler, which really does lack every phrase in
it, so the table stayed and the caller changed: a phrase is trusted only for
the tokens M1 lowers **nowhere** — `[`, `]`, `:`, `,`, and the lexer's own
`Unsupported` bucket — and every other position says what it wanted and what it
found. `else { }` now reads *"this engine needs an operand here, and found the
`else` keyword instead"*.

What is left is narrower and stays recorded in `control_conformance.rs`: a
phrase that is **true** can still be the wrong answer for the position.
`catch (e, f)` says "does not support the comma operator", which is true of the
engine and is not the reason a CatchParameter may not be two bindings.

Two bounds are the engine's rather than the language's, and say so the same way:
syntax nested past the compiler's frame budget (a stack overflow is a process
abort, so the depth is a number the compiler keeps), and more than 64 `$N`.

## Reaching a host, with arguments

A script that can only compute is not much use, so `Names::Declared` is the
door out. An embedder declares the raw wasm functions it has, and how a
JavaScript argument maps onto their parameters:

```rust
use tinyvm_qjs::{HostFn, HostParam, HostResult, Names, Options, compile_qjs_m1_with};

let table = vec![
    // sys.call(op_ptr, op_len, args_ptr, args_len) -> i32
    HostFn {
        name: "call".into(), module: "sys".into(), field: "call".into(),
        params: vec![HostParam::StrPtrLen, HostParam::StrPtrLen],
        result: HostResult::I32,
    },
    // sys.result_len() -> i32  and  sys.result(dst, cap) -> i32,
    // written `result()` in a script and answering with a String.
    HostFn {
        name: "result".into(), module: "sys".into(), field: "result".into(),
        params: vec![],
        result: HostResult::Bytes { length: "result_len".into() },
    },
    // sys.print(ptr, len) -> ()
    HostFn {
        name: "print".into(), module: "sys".into(), field: "print".into(),
        params: vec![HostParam::StrPtrLen],
        result: HostResult::Void,
    },
];
let wasm = compile_qjs_m1_with(
    r#"if (call("spawn", "{}") === 0) { print(result()); } return 0;"#,
    Options { names: Names::Declared(table) },
)?;
```

**The compiler unwraps; the door does not learn about JavaScript values.** The
emitted module imports `sys.call(i32,i32,i32,i32)->i32`,
`sys.result_len()->i32`, `sys.result(i32,i32)->i32` and `sys.print(i32,i32)`,
and nothing else — the same import table a hand-written `.wasm` guest would
present. That direction is the whole design: a door speaking `(tag, payload)`
would break every hand-written guest and leak one language's value
representation into a boundary meant to serve any guest. So this crate owns the
*mechanism* and names nobody's host function; the vocabulary is the embedder's.

- A **String** argument lowers to `(ptr, len)` — the UTF-8 bytes in linear
  memory, valid for the call only, since the guest heap is a bump allocator.
- A **Number** lowers to `i32` or `f64` as the declaration says. `i32` means
  the Number has to *be* one: a fractional value, a NaN, an infinity or
  anything out of range traps rather than being rounded.
- A **`Bytes`** result is the two-pass read: ask the length, bump-allocate a
  string record on the guest's own heap, ask for the copy, and check *two*
  things — that the announced length is a length at all, and that the copy wrote
  what it promised. The second check alone is not enough, because it compares
  one host answer to another: a host that answers `-1` to both (the raw
  contract's "your buffer is too small") passed it, and produced a String whose
  length header read 4 GiB. `__alloc` rounds with `(n + 3) & -4`, which is
  negative for a negative `n`, so repeating that walked the bump pointer
  *backwards* over the fault word and made the guest report an exhausted heap
  for a script that merely had a type error. A short, long or negative answer
  traps.
- A **wrong type** is a compile diagnostic where the compiler can settle it
  (`print(1)`), and a trap where only the run can (`print(x)`).
- Only the declarations a script mentions become imports, in **declaration
  order**, so an embedder can predict its import table without reading the
  script.

## Three reasons a guest stops, and how a host tells them apart

The bump heap grows linear memory; the host's `Limits` is what bounds it. When
that bound is reached, `memory.grow` returns `-1` — standard wasm, not a trap
(`crates/tinyvm/src/wasm.rs`, `Op::MemoryGrow`) — so the refusal carries no
reason and the allocator has nowhere to put one. It falls into `unreachable`,
which is the same instruction a conversion this milestone lacks executes, and
the same one an uncaught `throw` ends at. So the host would receive the same
`WasmError`, the same `"unreachable executed"` and the same
`FaultClass::Guest` for three entirely different situations: a script that ran
out of budget, a script that is simply broken, and a script that threw and
terminated exactly as ECMA-262 says it should.

Each wants a different answer from the host — raise the ceiling, report the
defect, report the exception — so guessing between them is exactly the
misclassification worth avoiding. The guest writes the reason down first, in
the first word of its own linear memory, an address the bump pointer never
hands out. Read it after a trap:

```rust
use tinyvm_qjs::{GuestFault, guest_fault};

if let Err(fault) = instance.invoke_by_name("main", &tinyvm_qjs::Value::args(&[])) {
    match guest_fault(&instance.memory().expect("memory zero")) {
        Some(GuestFault::HeapExhausted) => { /* budget: raise max_memory_pages */ }
        Some(GuestFault::UncaughtThrow) => { /* the script threw; report it */ }
        _ => { /* the script itself: report `fault` */ }
    }
}
```

`None` means the guest recorded nothing — an ordinary guest fault, or a module
with no linear memory at all. The entry point clears the word on the way in, so
the answer is about the call that just failed rather than an older one; it
clears the unwind channel's flag beside it, for the same reason and after the
same defect.

The word is only trustworthy if the bump pointer can never reach it, so that
is a check in `__alloc` and not a comment: an allocation that did not move the
pointer forward is refused. Rounding does not preserve "forward" on its own — it
is negative for a negative size and negative again for a size within three of
`i32::MAX` — and the guest reachable way to get one of those was a host's
`Bytes` length. Nothing is written to the fault word on that path: a size the
allocator cannot represent is not a budget anyone can raise, and calling it
`HeapExhausted` would be the same misclassification pointing the other way.

No import, no export, and nothing for a host to opt into: a module with no
declared host imports still fails honestly. The cost is 23 bytes of emitted
wasm per module, whatever the script's size — seven where the allocator gives
up, seven where the entry point clears the word, and nine for the check that
keeps the word out of the allocator's reach.

## The M0 skin

```rust
// A bare name resolves to nothing; the diagnostic says which construct is ahead.
tinyvm_qjs::compile_qjs("$0*2")                       // -> Result<Vec<u8>, CompileError>

// The skin. `g` and `g()` both call the zero-argument import `js.g`.
tinyvm_qjs::qjs2wasm("g()+$0")                        // -> Result<Vec<u8>, WasmError>
tinyvm_qjs::eval_qjs("g()+$0", &globals, &locals)     // = eval_wasm(&qjs2wasm(src)?, ..)
```

The difference is one field of `Options` — see `Names`. The world of the skin is
only the two `eval_wasm` bindings: `globals` is the import table a name resolves
against, `locals` are this call's `$N`. A host call there takes no arguments;
the declared table above is the third world that gives it some.

`CompileError` carries a `String`; `WasmError` carries a `&'static str` because
the core is fmt-free. `qjs2wasm` narrows one to the other by declared
`Boundary`, never by re-reading the sentence.

Commissar demo (from repository root):

```sh
cargo run -p tinyvm-qjs --example commissar
```

## The downstream binding library: it compiles, and it runs

`agenterm/scripts/qjs/lib/fleet.js` is the acceptance target: 231 lines,
6 280 bytes, that wrap one host call in a tree of namespace tables. It used to
stop at **line 14, byte 727**, on the conditional in `call()`. It does not stop
anywhere.

**Verbatim, whole**: 6 280 source bytes → **22 457 bytes** of wasm, clearing
tinyvm's load gate, instantiating, with 29 function-valued properties reachable
and exactly **one** import, `js.__host`. It was 16 400 with `JSON` resolving to
an opaque `js.JSON` import that no host could answer; the 4 535 it grew by is
the JSON set and the unwind channel `JSON.parse` raises through, which arrive
together because a program that names `JSON` needs both.
`the_whole_fleet_library_compiles_and_its_methods_are_reachable` and
`fleet_js_compiles_verbatim` in `tests/function_values.rs` are the evidence,
and the second one asserts the two constructs are still spelled in the snapshot
the way the library spells them — so the test says the engine reads them rather
than that somebody rewrote the file.

Compiling is one claim. `tests/fleet_acceptance.rs` holds the other, which is
the one an embedder needs: seven tests that drive `fleet.js`'s own `call()`
through a real raw host door to a broker answering JSON, and read a property
off the parsed answer.

```js
fleet.tabs.set_note = function (tabId, note) {
  return call("tabs.set-note", JSON.stringify({ tab: tabId, note: note }));
};
return fleet.tabs.set_note("t3", "ship it").ok;   // -> true
```

The host saw `("tabs.set-note", {"tab":"t3","note":"ship it"})` — the params
JSON written by this engine — and answered `{"ok":true,"tab":"t3"}`, which this
engine parsed. Six capabilities have to hold at once for that line: the object
literal, the function value in a property, the call through it, the conditional
that supplies the default argument, `JSON.stringify` out and `JSON.parse` back.

**One reduction, and it is named rather than glossed.** `fleet.js` reaches its
door as `__host.fleet_call(op, params)` — a property call on a **free** name.
Under `Names::HostImport` a free name is a zero-argument `js.*` import
answering one V1 pair, and no host can answer that pair with an Object:
`Value` has no Object variant, and building an object record in guest memory by
hand would mean the host knowing this engine's record layout, which is the leak
the raw door exists to prevent. So the embedder supplies `__host` itself, in a
short prelude:

```js
const __host = { fleet_call: function (op, p) { return door(op, p); } };
function door(op, p) { fleet_call(op, p); return fleet_result(); }
```

— where `fleet_call` and `fleet_result` are two `Names::Declared` raw doors,
the second a `Bytes` result so the answer comes back as a String. Two and not
one because the raw contract is a status code plus a two-pass read, which is
the shape a variable-length host answer has and not something this wrapper
invented. Closing that gap properly means a way to declare an *object-shaped*
host namespace, and that is a decision about the host boundary rather than
about this library.

One behaviour a caller will meet, and it is not a defect in the parser:

| | |
| --- | --- |
| `o.m()` inside a wrapper | the function `o.m` holds is called and it cannot see `o`; there is no `this` yet. Inert for every method in this library, and what the `this` milestone has to fix |

The other row here used to be the array one — a broker answer that was or
contained a JSON array took the `catch` and came back as raw text. The Array
milestone removed it; `tests/fleet_acceptance.rs::an_array_answer_is_a_list_the_caller_can_index`
is the same shape asserting the new answer.

## Who consumes it

tinyvm's own acceptance suite, and AgenTerm's `agenterm-qjswasm`, which pins
this crate by git revision. The split is the layering rule: generic
dynamic-engine capability lives here, an embedder's host door and slot policy
live in the embedder.
