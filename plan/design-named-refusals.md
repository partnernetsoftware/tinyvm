# 剩下的四个脚本走得到的 `unreachable`，各有名字（A11 b/c/d）

| 日期 | 2026-08-30 |
|---|---|
| 目的 | 把 `crates/tinyvm-qjs` 里脚本能走到的最后四个无名 trap 变成有名字的拒绝 |
| 前置 | `design-to-string-of-objects.md`（fault 9 的同一套机制）、PRD A11 |

## 分两堆，不是一堆

四个停点性质不同，硬塞进一个 fault 码会把「脚本写错了」和「引擎表示不了」混在一起，
下游 agenterm 的 `exit_class` 正是靠这个区分（`script` vs 能力边界）。

**脚本自己的事 → `FAULT_INVALID_WRITE = 10`**，`FAULT_THROWN` 指向池里的理由：

| 脚本 | 理由串 | 落点 |
|---|---|---|
| `a["x"] = 1`、`a[1.5] = 1`、`a[-1] = 1`、`a[16777216] = 1` | `an Array key that is not an index below 16777216` | `array.rs` `prop_set`，`__arr_index` 答 −1 的那条臂 |
| `s[0] = "x"`、`n[0] = 1`（有数组的程序） | `a property write on a value that has no properties` | `array.rs` `prop_set`，接收者既非 Object 也非 Array |
| 同上（没有数组的程序，所有成员写都走 `__obj_set`） | 同上 | `runtime.rs` `obj_set`，`unbox_object` 之前 |

ECMA-262 对前一类会把键挂成具名属性、对后一类在 sloppy 模式下静默忽略——两个都是「静默」，
和 fault 9 拒绝 `[object Object]` 是同一条原则：这引擎不静默。

**引擎表示不了 → 还是 `FAULT_CAPABILITY = 3`，但带名字**（`guest_capability_name`）：

| 脚本 | 理由串 | 落点 |
|---|---|---|
| `"ab".split("")` | `split with an empty separator` | `method.rs` `split`，`nl == 0` |
| `"😀x".slice(1)`、`"a😀".slice(0, 2)` | `a slice boundary inside a surrogate pair` | `method.rs` `slice_core`，`from`/`to` 落在代理对中间 |

UTF-8 表示不了孤立代理项，这两个不是「没做」而是「做不了」（`split` 的文档注释早就这么说）。
老的无名 capability 臂（没写 `.length` 的程序里缺失的 String 属性）现在**主动清零** `FAULT_THROWN`，
所以同一个实例上先撞一个有名字的、再撞一个无名的，宿主读不到旧名字（`refused_operations.rs` 钉着）。

## 门控

名字只在能走到这些臂的程序里进池：`scan.member_write`（任何通过成员表达式的赋值/自增，新扫描位）
或 `Me::Split` 或 `Me::SliceCore`。没有成员写、不 split、不 slice 的程序字节不变。

## 没做的

- `arr_set` 里 `index >= MAX_INDEX` 的守卫**故意留无名**：脚本每条路都先经过 `__arr_index`，
  它已经拒绝了；只有引擎内部的调用者能到那里，是引擎的缺陷不是脚本的。
- 引擎自证的那一堆（没有第九个 tag、手造对守卫、门控关掉时的空身体）仍是 PRD A11 的
  `FAULT_ENGINE = 10`→ 改为 11 的候选（10 已被本页占用）。
