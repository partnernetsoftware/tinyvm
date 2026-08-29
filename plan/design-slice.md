# `String.prototype.slice`

| | |
|---|---|
| 日期 | 2026-08-29 |
| 状态 | 已落地：`method.rs` `Me::{Slice, SliceFrom, SliceCore}`，`tests/slice_m3.rs` |
| 需求 | 下游迁移映射把 rh 的 `sub_string(a, b)` 写成 `slice(a, b)`；`test_harness.bounded_record_text` 用它截断证据；第一波六组全部点名 |

## 形状

三个 `Me`：`Slice`（两参）、`SliceFrom`（一参，`end = length`）、`SliceCore`
（内部，`(record, from_units, to_units) -> record`）。两种调用形是几条指令包着同一个
核心，所以一个程序两种都用只付一份：**756 B / 647 B / 两者 1 029 B**（相对只用
`.length` 的程序）。`SliceCore` 一趟走字节：前导字节算一个码元、4 字节序列算两个；
到达 `from`/`to` 记下字节偏移，再交给 `Substr` 拷贝。

## 语义与收窄

- 位置是 UTF-16 码元（与 `length`、`indexOf` 同一把尺）。
- 索引必须是 Number：非 Number 在 `unbox_number` trap——本引擎方法参数一贯的收窄，
  不做 ToIntegerOrInfinity 的类型转换。NaN = 0；**先夹到 `[-len, len]` 再截断**，
  截断因此不可能 trap，而夹取在范围内不改变任何值；负数从尾部数。
- 边界落在代理对中间 = 孤立代理，UTF-8 表达不了：**trap**，与 `split("")` 同一条
  理由（unrepresentable rather than unimplemented）。

## 零成本

`"return 1;"` 仍是 9 765 字节；`what_slice_costs_is_written_down` 钉住。

## 踩到的

`Plan::want` 只拉一层 helper；`SliceCore` 自己的 helper（`Units`、`Substr`）没进计划，
发射 prefab 体时 `offset()` panic，信息却写着「call site asked for a method the plan
does not carry」——不是调用点，是 prefab 体。列表照 `ToLowerCase` 的惯例写平了。
