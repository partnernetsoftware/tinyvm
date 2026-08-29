# `"" + n`：整数走位数循环，不走 Dragon4

| | |
|---|---|
| 日期 | 2026-08-29 |
| 状态 | 已落地：`convert.rs` `num_to_string` 前置快路径，`tests/num_to_string_fast.rs` |
| 起因 | 下游 CLI 二分 `--max-operations` 实测：`"" + n` 每次 **≈5 200 步**，是迁移脚本每行最贵的一件事（tinyvm PRD A9） |

## 形状

Number::toString 步骤 6–7 在 `k == n` 时就是「数字本身」。`|x| < 2^31` 且 `x == trunc(x)`
的整数不再进 Dragon4：`i32.trunc` 后先数位数（至少一位，0 印 `"0"`），再从尾部往前写
`n % 10 + '0'`，负号在前。-0 仍由步骤 2 答 `"0"`；`2^31` 及以上、分数、NaN、±Infinity
走原路，答案与 Rust 的 `format!` 逐一相同（`integers_print_as_their_digits`）。

## 代价

`__num_to_string` 在无条件运行时里，所以**每个程序 +175 B**（`"return 1;"` 9 765 → 9 940），
13 条字节门同时上移，理由写在 `arrays_m3.rs` 的历史块里。换来 `"" + 12345` **537 步**
（此前 ≈5 200）。没有门可开：任何带 `+` 的程序都可能把数字转成字符串。
