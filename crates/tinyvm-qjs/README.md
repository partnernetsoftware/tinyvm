# tinyvm-qjs

The `.qjs` → `.wasm` compiler, in pure Rust, plus the language skin over
[`eval_wasm(data, globals, locals)`](../tinyvm). Not rquickjs, not a QuickJS C
binding, and not a JS engine yet.

```text
source  --lex-->  tokens  --parse-->  AST  --emit-->  wasm IR  --encode-->  bytes
```

Five stages, five modules. The encoder is hand-written on purpose: the output
has to clear tinyvm's load gate, which is strict about canonical section order,
minimal LEB128 and exact expression termination, so this crate owns that
correctness instead of assuming it from a dependency.

## Two faces

```rust
// The language. A bare name resolves to nothing yet, and the diagnostic says
// which construct is ahead of the engine, and where.
tinyvm_qjs::compile_qjs("$0*2")                       // -> Result<Vec<u8>, CompileError>

// The skin. `g` and `g()` both call the zero-argument import `js.g`.
tinyvm_qjs::qjs2wasm("g()+$0")                        // -> Result<Vec<u8>, WasmError>
tinyvm_qjs::eval_qjs("g()+$0", &globals, &locals)     // = eval_wasm(&qjs2wasm(src)?, ..)
```

The difference is one field of `Options` — see `Names`. The world of the skin is
only the two `eval_wasm` bindings: `globals` is the import table a name resolves
against, `locals` are this call's `$N`. A host call takes no arguments; that
would need a third world.

Subset today: decimal integers, `+ - * / %`, unary minus, grouping, `$N`.
Everything else is rejected with "this engine does not support X yet" and a byte
offset — a boundary a reader can see, not one they have to guess at.

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
