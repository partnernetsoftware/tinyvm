# 非原始值的 ToString / ToNumber：有名字的拒绝，不是答案

| | |
|---|---|
| 日期 | 2026-08-30 |
| 目的 | `"" + {}`、`o[{}]`、`"" + [1,2]`、`"" + f` 在 `__to_string` 的最后一臂 `unreachable`——A11 (a)，脚本走得到的无名 trap 里最常见的一个 |
| 位置 | `runtime.rs` `to_string` / `ObjectNames` / `FAULT_NO_PRIMITIVE_FORM = 9`；`emit.rs` `Scan.objects`；`lib.rs` `GuestFault::NoPrimitiveForm` / `guest_no_primitive_form`；钉在 `tests/to_string_of_objects.rs` |

## 先做错的那一版

第一版按 ECMA 给答案：`[object Object]`、`join(",")`、`"function"`。跑套件时十一条既有测试反过来
（`a_function_is_never_quietly_converted`、`an_array_has_no_primitive_form`、`an_object_used_as_a_key_faults`、
`arithmetic_on_an_object_traps`……分布在五个文件）——它们钉的是引擎一条**有意的原则**：对象、数组、
函数**从不静默转换**。理由是脚踏枪：一个对象悄悄变成 `[object Object]` 进了命令行或当了键，
比一次停机贵得多；`JSON.stringify` 才是「我要它的文本」的拼法。A11 的题是**给 trap 一个名字**，不是推翻原则。

## 现在的形状

`to_string` **和 `to_number`** 对 Object / Array / Function 三个 tag 各一臂：`FAULT_THROWN` = 池里的种类名
（`an Object` / `an Array` / `a function`），fault 字 = 9（`FAULT_NO_PRIMITIVE_FORM`），`unreachable`。
`f + 1`、`o * 2`、`-o`、`f < 1` 走的是 ToNumber，同一个名字。不可捕获——与 `MissingStringMethod` 同类，
不是 ECMA 的 TypeError（ECMA 那里根本不出错）。宿主读 `guest_no_primitive_form` 得到种类。

## 门控

三个池串与三条臂只在程序**能持有**非原始值时发射：`scan.objects`（新扫描位：对象字面量）∨ `arrays` ∨ `json` ∨
`function_values`。只有原始值的程序到不了那一臂，`"return 1;"` 不动；有对象的程序 +120 字节。

## 数字

| 程序 | 前 | 后 |
|---|---|---|
| `"return 1;"` | 10 025 | 10 025 |
| `let o = {a:1}; …` | 10 193 | 10 313 |
| `return JSON.stringify({a:1});` | 16 570 | 16 691 |
| fleet 库 | 23 816 | 24 003 |
