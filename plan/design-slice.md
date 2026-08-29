# `String.prototype.slice`

| | |
|---|---|
| 日期 | 2026-08-29 |
| 状态 | 已落地：`method.rs` `Me::{Slice, SliceFrom, SliceCore}`，`tests/slice_m3.rs` |
| 需求 | 下游迁移映射把 rh 的 `sub_string(a, b)` 写成 `slice(a, b)`；`test_harness.bounded_record_text` 用它截断证据；第一波六组全部点名 |

## 形状

三个 `Me`：`Slice`（两参）、`SliceFrom`（一参，`end = length`）、`SliceCore`
（内部，`(record, from_units, to_units) -> record`）。两种调用形是几条指令包着同一个
核心，所以一个程序两种都用只付一份：**832 B / 682 B / 两者 1 138 B**（相对只用
`.length` 的程序；第二版比第一版多 ~76 B，换来的是下面的「懒长度」）。`SliceCore` 一趟走字节：前导字节算一个码元、4 字节序列算两个；
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

## 第二版（同日）：懒长度

第一版先算整串的码元长度再夹取，`s.slice(0, 10)` 对 1 000 字符的串量到 **78 000 步**
（CLI 二分 `--max-operations`）。ECMA 的顺序是先截断再看符号；非负索引根本不需要长度——
核心把「越过末尾」当末尾——所以只有负索引才数长度，且到那时才数。核心找到 `to` 就跳出
循环（第一次写成 `br 2`，跳到的是 loop 标签 = continue，步数预算把它拦下来了）。
现在同一调用 **< 3 000 步**（`a_non_negative_slice_does_not_walk_the_whole_string` 钉住）。
