# 调用非函数：一个有名字的拒绝

| | |
|---|---|
| 日期 | 2026-08-30 |
| 目的 | `undefined()`、`o.missing()`、`[].concat()`（引擎没有的方法读回 `undefined`）都只答 `guest trapped: unreachable executed`；下游 lint 第一次跑就撞上 |
| 位置 | `runtime.rs` `build_call_check` / `FAULT_NOT_A_FUNCTION`；`emit.rs` `call_checked_record` / `env_of_callee`；`lib.rs` `GuestFault::NotAFunction` / `guest_not_a_function`；钉在 `tests/not_a_function.rs` |

## 形状

每个间接调用点（`indirect_call` 与 `specialised_method` 的非 String 分支）原来是
`unbox_function` 的 `require_tag(TAG_FUNCTION)` → `unreachable`。现在：

1. 先读环境字（有捕获的程序）——**不测标签**：地址乘以「是函数」，非函数读的是地址 0 的 `FN_ENV` 字
   （fault 区，无害），所以不会在参数求值前先 trap；
2. 求参数（ECMA-262 13.3.6.1：参数先于可调用性检查，`indirect_attack::arguments_run_before_the_tag_test_faults` 钉着）；
3. `__call_check(tag, payload, name) -> record`：是函数就答记录（保留 `unbox_function` 的高半字校验）；
   不是——有 unwind 通道时把 `"TypeError: <name> is not a function"` 挂上通道，**答一个蹦床记录**：
   蹦床是 uniform 签名、答 `undefined` 的适配器，占 JSON 适配器之后的那个表元素；
   没有通道时写 fault 8、`FAULT_THROWN` = 名字、`unreachable`；
4. 调用照常进行（进蹦床），调用点原有的 `throw_check` 带着正确的栈形状离开。

第 4 步是这个设计的要点：`throw_check` 只能在「调用结果对在栈上」的地方分支，参数压栈之后、调用之前
没有任何合法的分支点——第一版把检查放在参数之前、直接分支，装载门报 `operand stack underflow`。

## 门控

`__call_check` 与蹦床只在 `scan.indirect`（程序有经表的调用）时发射：自己的一个集合，跟在方法集之后；
`"return 1;"` 一个字节没动。第一版把它塞进无条件运行时集，每个程序白背 19 字节——记忆宫殿那张
「放错房间」表里 `__len` 那一行的原样重演，抓住了。

## 名字

`Name` 调用点给源码里的名字，`o.f()` 给 `f`，其它给 `<expression>`；进字符串池，每个调用点一个常量。

## 数字

| | 前 | 后 |
|---|---|---|
| 有闭包的程序（`function mk…`） | 10 189 字节 | 10 342（+153：`__call_check` + 蹦床 + 两段池串） |
| 每个闭包调用点 | 99 字节 | 83（两次 `unbox_function` 换成一次环境读 + 一次调用） |
| `"return 1;"` | 10 025 | 10 025 |
