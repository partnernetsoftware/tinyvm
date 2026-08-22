# tinyvm PRD（产品占位）

实现仍在 `agenterm`：`crates/agenterm-tinyvm`、`crates/agenterm-tinyvm-qjs`。
成熟后把 agenterm 的 `prd/PRD_02_35_agenterm_tinyvm.md` 和 crate 文档 **upsert** 进本目录，替换占位。

## 产品句

便携可编程核。超级应用「跑程序」必须经此：`eval_wasm(data, globals, locals)`，装载期校验，宿主资源上限，无 JIT 也能活（含 iOS）。

语言皮（qjs 子集糖）在 `agenterm-tinyvm-qjs`：`eval_qjs = qjs2wasm + eval_wasm`。完整 JS / 容器 / 市场后加。

## 纪律

- 核只吃 wasm 字节。globals/locals 是宿主门，不是 POSIX。
- 一个 wasm 一个插件。插件只经宿主门，互不见。
- 不搬 V8 / workerd / QuickJS 源码。借 Cloudflare Workers 分层（隔离槽 / 语言皮 / 宿主门 / 上限在核 / 容器后加）。
- 槽 B（桌面 AOT 成本机码）停。dyn / #78 不进本产品。
- 写刀仍在 agenterm，本仓不接刀。

## 笔记

- [底层性能](notes-performance.md)
