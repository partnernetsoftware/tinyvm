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

## 第二刀（2026-08-31）：位数循环覆盖整个安全整数区

下游 server-smoke 剖面（agenterm `plan/design-host-op-budget.md` §7）：记录里的 13 位毫秒时间戳
`1788101436756` 超出 2^31，离开位数循环走 Dragon4，一个 **32 786 步**（`JSON.stringify` 里 32 567）；
旅程序列化 102 个 ≈3.3M 步，12%。

指令集没有 i64 除法（`repr.rs` 只有 `I64Const/Load/Store/Eq` 与 reinterpret），所以在 f64 里拆：
`hi = trunc(x / 1e9)`，`lo = x - hi * 1e9`，两半都 < 2^31，各走同一个 i32 位数循环；`hi != 0` 时
`lo` 恰好九位补零。所有量都是 < 2^53 的整数，乘减都精确；商是正确舍入不是截断，`x` 紧贴 1e9 倍数下方时
可能大 1，`lo < 0` 说出来就修（`integers_beside_a_multiple_of_a_billion_print_as_their_digits` 钉
每个倍数两侧）。快路径条件 `|x| < 2^53 && x == trunc(x)`；2^53 本身、1e18、分数、NaN、±Infinity 仍走 Dragon4。

| | 前 | 后 |
|---|---|---|
| `"" + 12345` | 553 | 590（多一次除、一次乘减、两次 trunc） |
| `"" + 2147483647` | 737 | 617 |
| `"" + 1788101436756` | 32 786 | **797** |
| `"" + 9007199254740991` | 41 811 | 846 |
| `JSON.stringify({a: 1788101436756})` − `{a: 1}` | 32 567 | **541** |
| journal 记录一条 | 82k | 51k |

代价：每个程序 **+191 B**（`"return 1;"` 10 007 → 10 198），十四张钉表同移；理由在 `arrays_m3.rs`。

