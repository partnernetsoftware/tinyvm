# tinyvm PRD（产品占位）

实现仍在 `agenterm`：`crates/agenterm-tinyvm`、`crates/agenterm-tinyvm-qjs`。
成熟后把 agenterm 的 `prd/PRD_02_35_agenterm_tinyvm.md` 和 crate 文档 **upsert** 进本目录，替换占位。

## 产品句

政委 2026-08-22：做得好是刚性需求。自己要一颗融合 qjs+wasm 的跨架构引擎，像自己的 JVM/V8：写一次，多平台，高性能。

便携可编程核。超级应用「跑程序」必须经此：`eval_wasm(data, globals, locals)`，装载期校验，宿主资源上限，无 JIT 也能活（含 iOS）。
对齐 JVM（校验+上限+同语义），不对齐 V8（JIT+完整 JS）。qjs 糖是皮，wasm 是核。

语言皮在 `agenterm-tinyvm-qjs`：`eval_qjs = qjs2wasm + eval_wasm`。完整 JS / 容器 / 市场后加。

## 纪律

- 核只吃 wasm 字节。globals/locals 是宿主门，不是 POSIX。
- 一个 wasm 一个插件。插件只经宿主门，互不见。
- 不搬 V8 / workerd / QuickJS 源码。借 Cloudflare Workers 分层（隔离槽 / 语言皮 / 宿主门 / 上限在核 / 容器后加）。
- 槽 B（桌面 AOT 成本机码）停。dyn / #78 不进本产品。
- 测试优先：先验收测再改脸。套件跟着产品句长。工人自报不算过。
- 写刀仍在 agenterm，本仓不接刀。

## 笔记

- [底层性能](notes-performance.md)
