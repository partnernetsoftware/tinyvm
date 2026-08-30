# `indexOf` / `includes`：跳过没有首字节的四字节窗

| | |
|---|---|
| 日期 | 2026-08-30 |
| 目的 | 128 KiB 串上未命中 36–39 步/字符；下游 lint（13.9 MB × 7 次）与 prd-alignment（729 KB × 69 次）都撞死在 1G 步上 |
| 位置 | `crates/tinyvm-qjs/src/method.rs` `skip_clear_window` / `first_byte_pattern`；钉在 `tests/index_of_cost.rs` |

## 做了什么

位置循环顶部（边界检查之后）加一步：若还剩四字节，`i32.load` 一个窗，用 has-zero-byte 技巧
（`(x - 0x01010101) & ~x & 0x80808080`，`x` 是窗与 `first * 0x01010101` 的异或）判断窗里有没有针的首字节；
没有就 `i += 4` 继续，有就落回原来的逐字节验证。指令集没有 `xor` / `not`，
异或拼成 `(a|b)-(a&b)`，取反拼成 `-1 - x`（见记忆 `tinyvm-qjs-instruction-set-gaps`）。

## 数字

| | 前 | 后 |
|---|---|---|
| `includes` 未命中，128 KiB | 35.7 步/字符 | **7.2** |
| `indexOf` 未命中，128 KiB | 38.7 | **7.2** |
| `"return \"ab\".includes(\"a\");"` 字节 | 10 392 | 10 474（+82，只有调这两个方法的程序付） |

首字节在文本里很常见时（如针以 `\n` 开头、每行都命中一次窗）退回旧价格，不会更差。

## 没做的

- 八字节窗：指令集没有 i64 `and`，要拆两个 i32 载入，收益减半；先看够不够。
- 两字节前缀过滤（Boyer–Moore–Horspool 类）：常数更好，但要一张表，等有程序证明需要。

## 同一晚的两个亲戚

- `toLowerCase`：每个码点都走 decode / 映射 / encode，ASCII 上 393 步/字符。字节 < 0x80 直接算
  （`+ 32` 当且仅当 `A`..`Z`，写成 `((b - 65) <u 26) << 5`）一字节出，其余照旧。38 步/字符，
  `tests/to_lower_case_cost.rs` 钉 <50。
- `split`：位置循环与 `indexOf` 同款，同一个 `skip_clear_window`；无分隔符的段 73 → 26 步/字符
  （剩下的是每个位置的边界检查与每段一次 `Substr` 分配）。
