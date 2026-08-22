# tinyvm

便携可编程核。写一次，多平台，高性能。

`tinyvm` 是自有、跨平台、可预算的标准 WebAssembly 解释器：接收普通 `.wasm`，
在不生成或装载动态机器码的前提下完成 decode / validate / instantiate / execute，
宿主通过标准 import table 提供版本化能力。无 JIT 也能活（含 iOS）。
静态核 `staticcore` < 100 KiB，`no_std`、fmt-free。

对齐 JVM（校验 + 上限 + 同语义），不对齐 V8（JIT + 完整 JS）。**qjs 糖是皮，wasm 是核。**

产品文档：[prd/PRD.md](prd/PRD.md) · 执行计划：[plan/goal-tinyvm-ios-game-runtime.md](plan/goal-tinyvm-ios-game-runtime.md)

## 两颗 crate

| crate | 脸 |
| --- | --- |
| [`crates/tinyvm`](crates/tinyvm) | `eval_wasm(data, globals, locals)`，卡带/宿主门/上限/replay/iOS C ABI，`tinyvm` CLI |
| [`crates/tinyvm-qjs`](crates/tinyvm-qjs) | 语言皮：`eval_qjs = eval_wasm(&qjs2wasm(src)?, globals, locals)`。不是 JS 引擎 |

```rust
use tinyvm::eval_wasm;

let value = eval_wasm(&wasm_bytes, &[], &[])?;
```

## 开发

```sh
cargo test --workspace                 # 默认 feature 全套
cargo test --workspace --all-features  # 含 cartridge-trust / replay / catalog-publisher / std-host
cargo clippy --workspace --all-targets --all-features
cargo fmt --all

crates/tinyvm/measure-core.sh          # < 100 KiB 静态核门
crates/tinyvm/smoke-wabt-*.sh          # wabt 差分 oracle（需 wat2wasm / wasm-validate）
crates/tinyvm/smoke-ios-bridge.sh      # XCFramework + Swift package 链接（需 Xcode）
```

CLI：

```sh
cargo run -p tinyvm --bin tinyvm -- module validate FILE.wasm
cargo run -p tinyvm --bin tinyvm -- cartridge check FILE.wasm --json
cargo run -p tinyvm-qjs --example commissar
```

## 下游

- [`nostalgia-arcade`](../nostalgia-arcade)：TinyArcade iOS App，经 `scripts/prepare-tinyarcade-runtime.sh`
  从本仓相邻目录重建 XCFramework 与卡带。
- `agenterm`：下游 embedder，不再持有 tinyvm 源。

## 许可

MIT OR Apache-2.0。
