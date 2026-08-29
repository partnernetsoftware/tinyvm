# 宿主参数类型错在运行期报名字

| | |
|---|---|
| 日期 | 2026-08-29 |
| 状态 | 已落地：`runtime.rs` `FAULT_HOST_ARGUMENT`（第六个 fault code）、`emit.rs` `unwrap_args`、`tests/host_argument.rs` |
| 起因 | 每个脚本作者的第一个 `print(n)`：编译期拒不了（`n` 的类型是运行期事实），运行期是 `unbox_string` 的裸 `unreachable` |

## 形状

`unwrap_args` 对每个 `StrPtrLen` 参数：若源码里是 String 字面量，**什么都不测**（编译器已经知道，
比以前少了两次 `require_tag`）；否则一次 `is_string`，失败时先把池里的 `"<host>#<n>"`
（1 起数）写进 detail 字、再写 code 6、再 `unreachable`。宿主 `guest_host_argument` 读回。

## 代价

字面量参数位：**变小**。非字面量 String 参数位：约 +12 B。`Number`/`F64` 参数仍是
`unbox_number` 的裸 trap——下一条。

## 下游

agenterm 的 CLI 面：``host function `print` needs a String for argument 1; write `"" + x` ``。
