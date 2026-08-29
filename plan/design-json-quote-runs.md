# `JSON.stringify` 的引号串：按段复制

| | |
|---|---|
| 日期 | 2026-08-30 |
| 目的 | `__json_quote` 每字节：七个 `if` 比较 + 一次 `__jb_byte` 调用 ≈117 步；小对象序列化每对象 5 000 步里大半是键与串的引号 |
| 位置 | `crates/tinyvm-qjs/src/convert.rs` `json_quote`；钉在 `tests/json_stringify_cost.rs` |

## 做了什么

外层循环每轮先扫一段「原样输出」的字节——`>= 0x20` 且不是 `"`、`\`（多字节 UTF-8 的每个字节都落在这里），
整段用 `__jb_bytes(buf, ptr, n)` 一次搬走（它按四字节复制，`design-json-parse-fast.md` 的 `jb_take` 同款）。
截断扫描的那个字节走原来的逃逸臂（七个双字符逃逸、`\u00xx`、默认 `__jb_byte`），然后继续下一段。

扫描条件用 `(c < 0x20) | (c == 0x22) | (c == 0x5c)` 一次算出、一个 `if` 里 `br 2` 跳出内层循环：
指令集没有 `else`/`select`，两个 `if` 的拼法见记忆 `tinyvm-qjs-instruction-set-gaps`。

## 数字（进程内 `last_steps`）

| | 前 | 后 |
|---|---|---|
| 1 000 字符纯串，每输出字节 | 117 | **39** |
| 50 个 `{name, count, ok}`，共 | 254 311 | 224 381（≈4 500/对象） |
| `"return JSON.stringify({a:1});"` 字节 | 16 330 | 16 413（+83，只有 JSON 程序付；`"return 1;"` 不动） |

## 下一层

每对象 ≈4 500 步不在引号里：属性遍历、键的 `__json_quote` 调用开销、数字的 `num_to_string`、缓冲扩容。
要再降就得量这几项各占多少，本条不猜。
