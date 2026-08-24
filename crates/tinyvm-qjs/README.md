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
let module = WasmModule::from_bytes_with(&wasm, Limits::default()).ok().unwrap();
let mut instance = module.instantiate().ok().unwrap();
let out = instance.invoke_by_name("main", &Value::args(&[Value::Number(21.0)])).ok().unwrap();
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
  recursion and mutual recursion. Calls are direct: a callee has to be a name
  bound to a known function.
- **Operators**: every rung the ladder has — assignment and its compound forms,
  `||`, `&&`, `==`/`!=`/`===`/`!==`, `<` `<=` `>` `>=`, `+` `-`, `*` `/` `%`,
  prefix and postfix `++`/`--`, unary `+ - !`, and grouping. `&&` and `||`
  short-circuit; `+` concatenates when either side is a string. `%` is
  ECMA-262's remainder, with the sign of the dividend and exact for operands a
  rounded quotient would get wrong — `-6 % 3` is `-0` and
  `2147483647 * 2147483647 % 1000` is `608`. `typeof` answers with the
  ECMA-262 13.5.3 name of each of the five types this engine has, `typeof null
  === "object"` included; a name the source never declares is still refused
  before `typeof` sees it, because there is no global scope for it to be
  absent from.
- **ASI**: ECMA-262 12.10, split where the spec splits it. Rule 3 is a fact about
  the token stream and lives in the lexer; rules 1 and 2 need a parser and live
  in the parser; the `for`-header override lives where the grammar position is.

## What it does not compile, and how it says so

Objects, arrays, member access, closures that capture, function values, `class`,
`throw`/`try`, `for…of`, `break`/`continue`, `switch`, template literals, the
bitwise and shift levels, `?:`, the comma operator, `**`, `??`, BigInt, and
the numeric literal forms above.

Each rejection is a `CompileError` whose sentence names the *engine's* boundary
— "this engine does not support X yet" — never "syntax error", and carries the
byte offset where the construct starts.

That wording is a product requirement, not a style preference. A subset this
small rejects mostly perfectly good scripts, so a sentence blaming the author
would be a lie, and a sentence that names no boundary leaves the reader guessing
where the engine stops.

Two bounds are the engine's rather than the language's, and say so the same way:
syntax nested past the compiler's frame budget (a stack overflow is a process
abort, so the depth is a number the compiler keeps), and more than 64 `$N`.

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
against, `locals` are this call's `$N`. A host call takes no arguments; that
would need a third world.

`CompileError` carries a `String`; `WasmError` carries a `&'static str` because
the core is fmt-free. `qjs2wasm` narrows one to the other by declared
`Boundary`, never by re-reading the sentence.

Commissar demo (from repository root):

```sh
cargo run -p tinyvm-qjs --example commissar
```

## Who consumes it

tinyvm's own acceptance suite, and AgenTerm's `agenterm-qjswasm`, which pins
this crate by git revision. The split is the layering rule: generic
dynamic-engine capability lives here, an embedder's host door and slot policy
live in the embedder.
