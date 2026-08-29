# A missing String property names itself

| | |
|---|---|
| 日期 | 2026-08-29 |
| 状态 | 已落地：`crates/tinyvm-qjs/src/runtime.rs` `FAULT_MISSING_STRING_METHOD`，`tests/missing_method.rs` |
| 起因 | 下游 40 个脚本迁移（agenterm wave-1）的六个小组各自报告 `slice`、`substr`、`substring`「trap」——三份报告，一个原因 |

## 问题

`"ab".length` 是 `__obj_get` 对 String 接收者唯一会答的属性。其余一律 trap 而不是答
`undefined`——这个选择是对的：`"ab".slice` 在 ECMA-262 里是真函数，`undefined` 是穿着
正确答案衣服的错误答案。但 trap 有两副面孔，取决于程序**别处**有没有写 `.length`：

| 程序里有 `.length` 吗 | 以前 | 
|---|---|
| 有 | `store_fault(FAULT_CAPABILITY)` + `unreachable`：宿主知道是能力边界，不知道是哪个 |
| 没有 | 直接落进 `unbox_object` 的裸 `unreachable`：宿主什么都不知道 |

同一个脚本换个上下文就换一种失败法，而且两种都不说方法名。迁移小组用 `s.foo()` 探针
一次就能看出「所有未知方法都这样」，但没人打这一针——每个人都在自己撞到的那个名字上
（`slice`）报了一个 bug。本仓自己也一样：六轮探针才想到打 `s.foo()`。

## 方案

`__obj_get` 的 String 臂里，key 就在一个 local 里。写进 fault 区第二个字
（与 `record_thrown_string` 同一个字、同一种记录形状），再写第五个 fault code，再 trap。
宿主端 `guest_missing_string_method(memory)` 读回名字；`GuestFault` 加单元变体
`MissingStringMethod`（枚举仍是 `Copy`）。

## 零成本门

`__obj_get` 在无条件运行时里，所以臂的存在必须门控，否则每个程序多 23 字节
（第一版就是这样，`"return 1;"` 9765 → 9788，13 条字节门同时红）。
门 = 扫描期看到**任何非 `length` 的静态属性读**（`scan.string_member`），`JSON.parse`
之类以 `Res::Json` 为对象的成员除外（`JSON` 永远不是 String 接收者）。

| 程序 | 以前 | 现在 | 差 |
|---|---|---|---|
| `return 1;` | 9 765 | 9 765 | 0 |
| `let o = {a:1}; o.b = 2; return o.a;` | 9 886 | 9 909 | +23（能到达这条臂，才付这 23 字节） |
| `return "ab".includes("a");` | 10 085 | 10 108 | +23 |
| fleet 库 | 22 452 | 22 475 | +23 |
| `return JSON.stringify({a:1});` | 15 409 | 15 409 | 0（JSON 排除生效） |

只有 `.length` 的程序保留原来的臂（能力 fault，无名字）：它到不了任何别的 key。

## 没做的

- 对象接收者上「属性不是函数」（`obj.missing()`）仍是裸 trap；ECMA 里是可捕获的
  TypeError。这是下一条边界，需要 unwind 通道参与，另立 note。
- `undefined.x` 仍是不可捕获的 trap（wave-1 也报了）。同上。
- `slice` 本身：这条 note 只让它**报名字**；实现另计（`design-slice`，待立）。
