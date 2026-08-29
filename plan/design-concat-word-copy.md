# `a + b` on strings: copy by the word

| | |
|---|---|
| 日期 | 2026-08-30 |
| 目的 | `s = s + "x"` 在长串上一次 ≈8 800 步（CLI 口径）/ 17 178 步（进程内口径，1 000 字符），脚本拼输出全走这条路 |
| 位置 | `crates/tinyvm-qjs/src/runtime.rs` `copy_loop` / `copy_addresses`；钉在 `tests/concat_cost.rs` |

## 病灶

`__str_concat` 分配 `la + lb + 4`，然后两个 `copy_loop` 各**一字节一步**地搬两个操作数。
每次迭代 ≈17 条指令，1 000 字节就是 ≈17 000 步。`JSON.parse` 的 `jb_take` 早已按四字节搬
（`design-json-parse-fast.md`），拼接没有跟上。

## 做了什么

`copy_loop` 改成两段：`i + 8 <= len` 时 `i64.load`/`i64.store` 一步八字节（对齐提示 0，
MVP 允许任意地址），然后逐字节收尾。地址计算抽成 `copy_addresses`，两段共用。
仍是循环而不是 `memory.copy`：bulk memory 是 post-MVP，产物要过 tinyvm 的 MVP 载入门。

## 数字

| | 前 | 后 |
|---|---|---|
| 1 000 字符 + `"x"`（进程内，`last_steps`） | 17 178 | **2 569** |
| `"return 1;"` 字节 | 9 940 | 10 025（**+85**） |

+85 是每个程序都付的：`__str_concat` 不受门控（`+` 作用于串是通用路径，没有可依据的文本信号），
所以 11 个钉表的每一行同移 85。接受的理由与 `num_to_string` 的 +175 相同：换来的是通用路径 6.7×。

## 没做的

- 就地追加（`a` 是堆顶块时直接延长）：串是不可变值，`t = s; s = s + "x"` 里的 `t` 会跟着变，
  除非表示改成 `(ptr, len)` 共享前缀——那是整个字符串表示的改动，不在这一层。
- 门控 `__str_concat`：要么按「程序里有 `+` 且某个操作数可能是串」扫描，要么接受 85 字节。选了后者。
