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
  string record on the guest's own heap, ask for the copy, and check that the
  copy wrote what the length promised. A short or negative write traps instead
  of producing a String with a fabricated tail.
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

No import, no export, and nothing for a host to opt into: a module with no
declared host imports still fails honestly. The cost is 14 bytes of emitted
wasm per module — seven where the allocator gives up and seven where the entry
point clears the word — whatever the script's size.

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

## Who consumes it

tinyvm's own acceptance suite, and AgenTerm's `agenterm-qjswasm`, which pins
this crate by git revision. The split is the layering rule: generic
dynamic-engine capability lives here, an embedder's host door and slot policy
live in the embedder.
