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
  and the script's ECMA-262 completion value.
- **Functions**: declarations and expressions, named or not, with parameters,
  recursion and mutual recursion -- and a function is a **value**. It can be
  stored in a binding or a property, passed, returned, and called from
  wherever it ended up: `o.m()`, `o.a.b()`, `f()()`. `typeof` answers
  `"function"`, every one of them is truthy, and `===` on two of them is
  identity — where *identity* means ECMA-262 15.2.5's: each **evaluation** of
  a function expression is a new object, so `mk() === mk()` is `false` and
  reading one binding twice is `true`. A call with too few arguments passes `undefined` and one with too
  many evaluates and discards the surplus. Calling something that is *not* a
  function **traps** -- ECMA-262 makes it a TypeError and there is no `throw`
  here -- and it traps at the tag test, before any table is reached. Two
  things a function value still is not: it has no `this` (so `o.m()` calls the
  function `o.m` holds and the function cannot see `o`), and it has no
  prototype (so `f.call`, `f.bind` and `f.length` are a trap and not a
  method).
- **Operators**: every rung the ladder has — assignment and its compound forms,
  `||`, `&&`, `==`/`!=`/`===`/`!==`, `<` `<=` `>` `>=`, `+` `-`, `*` `/` `%`,
  prefix and postfix `++`/`--`, unary `+ - !`, and grouping. `&&` and `||`
  short-circuit; `+` concatenates when **either** side is a String, running
  ToString on both — ECMA-262 13.15.3 step 1.d, and see "The three
  conversions" below. `%` is
  ECMA-262's remainder, with the sign of the dividend and exact for operands a
  rounded quotient would get wrong — `-6 % 3` is `-0` and
  `2147483647 * 2147483647 % 1000` is `608`. `typeof` answers with the
  ECMA-262 13.5.3 name of each of the five types this engine has, `typeof null
  === "object"` included; a name the source never declares is still refused
  before `typeof` sees it, because there is no global scope for it to be
  absent from.
- **Objects**: literals (`{}`, `{ a: 1 }`, shorthand `{ a }`, a trailing comma,
  and string- or number-literal keys), property reads by dot and by computed
  key, and property assignment including the compound and update forms —
  `o.a += 2`, `o.a++`. Keys are Strings, so `o[1]` and `o["1"]` are one slot
  and `o[0.5]` is the property `"0.5"`;
  a property that is not there reads `undefined` rather than trapping;
  property order is insertion order; and `===` on two Objects is reference
  identity. Reading a property *of* a primitive (`"abc".length`, `(1).a`)
  traps: there is no prototype here, and answering `undefined` would be a right
  answer by a wrong route for exactly the members a script reaches for.

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

A script that makes no function a value emits **no table, no element segment
and no adapter**. What every script pays is the growth law's price of a seventh
type — one arm appended last in each of `__typeof`, `__truthy` and
`__to_number` — plus 29 bytes for `__fn_new`. Measured on this crate's own
encoder: a function-valued property costs about **128 bytes** (the function,
its adapter, its element and the assignment), a call through a value costs
about **70 bytes** where the direct call it replaces costs about 28, and
`fleet.js` in whole comes to 16 381.

## What it does not compile, and how it says so

Arrays, closures that capture, `this`, arrow functions, `class`,
`throw`/`try`, `for…of`, `break`/`continue`, `switch`, template literals, the
bitwise and shift levels, `?:`, the comma operator, `**`, `??`, BigInt,
`JSON`, and the numeric literal forms above.

Calling a value is no longer on that list, and one consequence is worth
stating because it changes what a diagnostic says: `Object.keys(o)` and
`JSON.stringify(o)` used to stop at the *call* and now stop at the *name*. The
engine can make the call; it has no binding named `Object` or `JSON` to make it
on, and there is no global scope for one to be in. `o.toString()` compiles and
traps instead, because the property is simply absent.

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
semicolon. The end of a statement now says what it was looking for and names
ECMA-262 12.10, and only two kinds of token keep a capability phrase there,
because for them it is true: a `,` or a `:` would have *continued* the
expression, and the lexer's `Unsupported` bucket is beyond the engine whatever
the lexeme is. The same debt at operand and header positions is still open and
is recorded in `conformance_m2.rs`.

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

## When the heap runs out, the guest says so

The bump heap grows linear memory; the host's `Limits` is what bounds it. When
that bound is reached, `memory.grow` returns `-1` — standard wasm, not a trap
(`crates/tinyvm/src/wasm.rs`, `Op::MemoryGrow`) — so the refusal carries no
reason and the allocator has nowhere to put one. It falls into `unreachable`,
which is the same instruction a conversion this milestone lacks executes, so
the host receives the same `WasmError`, the same `"unreachable executed"` and
the same `FaultClass::Guest` for a script that ran out of budget and a script
that is simply broken.

Guessing between them is exactly the misclassification worth avoiding, so the
guest writes the reason down first, in the first word of its own linear memory
— an address the bump pointer never hands out. Read it after a trap:

```rust
use tinyvm_qjs::{GuestFault, guest_fault};

if let Err(fault) = instance.invoke_by_name("main", &tinyvm_qjs::Value::args(&[])) {
    match guest_fault(&instance.memory().expect("memory zero")) {
        Some(GuestFault::HeapExhausted) => { /* budget: raise max_memory_pages */ }
        _ => { /* the script itself: report `fault` */ }
    }
}
```

`None` means the guest recorded nothing — an ordinary guest fault, or a module
with no linear memory at all. The entry point clears the word on the way in, so
the answer is about the call that just failed rather than an older one.

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

## How far the downstream binding library gets

`agenterm/scripts/qjs/lib/fleet.js` is the acceptance target: 231 lines that
wrap one host call in a tree of namespace tables. Compiling it whole stops at
**line 14, byte 727**:

```js
const resultJson = __host.fleet_call(opId, params === undefined ? "{}" : params);
//                                                              ^ byte 727
```

> this engine does not support conditional expressions yet

A byte offset is where the parser stops first, not how far it got: the parser
stops at the *first* refusal, so 727 of 6 280 is not 12% done. Which is why
what follows is measured by compiling fragments.

Compiles and runs today:

| fragment | |
| --- | --- |
| `const fleet = {}; fleet.ui = {}; fleet.ui.tabs = {};` | all 15 of its namespace tables, in one module |
| `fleet.ui.tabs.width = 40; return fleet.ui.tabs.width;` | nested member read and write |
| `function params(tab, note) { return { tab: tab, note: note }; }` | all 7 of its distinct parameter-object shapes |
| `if (params === undefined) { return "{}"; }` | the default-argument test, as a statement |
| `call("tabs.list", "{}")` | a call to a statically known name, including a declared host door |
| `fleet.tabs.list = function () { … };` | all **29** of its function-valued properties |
| `fleet.tabs.list()`, `__host.fleet_call(op, p)`, `JSON.parse(s)` | all **10** of its calls through a value |

With its two remaining walls written the way this engine spells them — the
conditional as an `if`, the `try` gone — the whole library compiles to
**16 381 bytes** and clears the load gate: 110 defined functions, a 30-element
table (29 adapters and the null element 0), and exactly the two imports
`js.JSON` and `js.__host`. That is
`the_whole_fleet_library_compiles_and_its_methods_are_reachable` in
`tests/function_values.rs`. It was 9 007 bytes before the conversions landed,
and 6 625 of the 7 374 it grew by is the conversion prelude every module now
carries.

Still refused — each row is the diagnostic the compiler actually prints:

| fragment | diagnostic |
| --- | --- |
| `params === undefined ? "{}" : params` | this engine does not support conditional expressions yet |
| `try { … } catch (_err) { … }` | this engine does not support the `try` keyword yet |

Those are the only two left. `JSON.parse`/`JSON.stringify` compile, and what
they need in order to **run** is a host: `JSON` resolves to an import under
`Names::HostImport`. The String conversions they used to also need are in.

## Who consumes it

tinyvm's own acceptance suite, and AgenTerm's `agenterm-qjswasm`, which pins
this crate by git revision. The split is the layering rule: generic
dynamic-engine capability lives here, an embedder's host door and slot policy
live in the embedder.
