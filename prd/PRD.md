# tinyvm PRD

Parent: [`~/repos/index.md`](~/repos/index.md) —— 跨仓记忆宫殿。本文是产品级宫殿；
同一套纪律：一个事实一个归属、只链不抄、判断被推翻留撤销记录、`[x]` 是**有证据的
承诺**而不是自我评价（本仓用 `prd_x_leaves_have_suite_edges` + `LEAF_TESTS` 执行这条，
它是这条纪律的金丝雀）。

便携可编程核。写一次，多平台，高性能。

Execution plan: [tinyvm as an iOS game runtime foundation](../plan/goal-tinyvm-ios-game-runtime.md)

笔记：[底层性能](notes-performance.md)

## 产品句

政委 2026-08-22：做得好是刚性需求。自己要一颗融合 qjs+wasm 的跨架构引擎，像自己的 JVM/V8：写一次，多平台，高性能。

便携可编程核。超级应用「跑程序」必须经此：`eval_wasm(data, globals, locals)`，装载期校验，宿主资源上限，无 JIT 也能活（含 iOS）。
对齐 JVM（校验+上限+同语义），不对齐 V8（JIT+完整 JS）。qjs 糖是皮，wasm 是核。

语言层在 `tinyvm-qjs`：`.qjs -> .wasm` 纯 Rust 编译器 + 皮 `eval_qjs = qjs2wasm + eval_wasm`。
容器 / 市场后加。

## 纪律

- 核只吃 wasm 字节。globals/locals 是宿主门，不是 POSIX。
- 一个 wasm 一个插件。插件只经宿主门，互不见。
- 不搬 V8 / workerd / QuickJS 源码。借 Cloudflare Workers 分层（隔离槽 / 语言皮 / 宿主门 / 上限在核 / 容器后加）。
- 槽 B（桌面 AOT 成本机码）停。dyn / #78 不进本产品。
- 测试优先：先验收测再改脸。套件跟着产品句长。工人自报不算过。
- 2026-08-22 起写刀在本仓。agenterm 不再持有 tinyvm 源，只作为下游 embedder。

## Product definition

`tinyvm` 是自有、跨平台、可预算的标准 WebAssembly 解释器，也是
TinyArcade 的运行底层。它接收普通 `.wasm`，在不生成或装载动态机器码的前提下完成
decode、validate、instantiate 和 execution；宿主通过标准 import table 提供版本化能力。

TinyArcade 是第一个 embedding 和持续验收负载，不是 VM 的语言边界。游戏便利能力必须
表现为标准 Wasm 或显式、版本化的 host import，不能演化成私有 opcode、私有卡带字节码
或只服务某一款游戏的解释器分叉。

目标结果是：iOS App 可以装载一枚经过审核的标准 `.wasm` 卡带，创建一个有界、持久的
instance，逐帧驱动游戏，安全 suspend/resume；单枚坏卡带只能失败自身，不能拖死、越界
分配或破坏 App。

## 记忆宫殿：一段 `.qjs` 走到宿主门的七个房间

树说「有什么」，图说「东西放在哪」。每个房间放一件**只属于它**的知识，判据是
**它随什么变**：随 JS 语言或 wasm 规范变的，在本仓；随某个 embedder 的业务变的，
不在本仓。这条判据就是编译器归属那次讨论的结论，也是 `compile_qjs` 至今不认识任何
embedder 词汇的原因。

```mermaid
flowchart TD
  subgraph LANG["tinyvm-qjs — 随语言与规范变"]
    R1["① lex / parse / AST<br/><i>a rejection names the engine, not the author</i><br/><i>templates fold to +, arrows to function<br/>expressions — neither gets a node</i>"]
    R2["② lower to V1<br/>eight tags · dispatch order<br/><i>Number, then String, then the rest</i>"]
    R3["③ runtime prelude<br/>bump heap · object and array records<br/>method bodies: trim indexOf push pop map<br/><i>gated: unused means unemitted</i><br/><i>per method, not per set</i>"]
    R4["④ encode .wasm<br/>standard bytes · LEB128"]
    R1 --> R2 --> R3 --> R4
  end

  subgraph CORE["tinyvm — bytes only"]
    R5["⑤ load gate<br/>validate · Limits"]
    R6["⑥ interpret<br/>no JIT · per-call budget<br/><i>steps / depth / slots</i>"]
    R5 --> R6
  end

  subgraph HOST["the embedder — not this repo"]
    R7["⑦ slot + host door<br/><i>Names::Declared, no vocabulary of ours</i>"]
  end

  SRC(["source .qjs"]) --> R1
  R4 --> R5
  R6 <--> R7
  R6 --> OUT(["Value::returned<br/>one V1 pair"])
  FW["fault word<br/>heap exhausted / uncaught throw"] -.->|"the guest writes its own reason"| R7
```

**房间 ③ 是最容易放错的一间**，而且有两种放错法。

**一、放进无条件的那一格。** `runtime::SET` 是无条件的——`Rt::offset()` 是它在那个切片
里的下标——所以往里加一臂就是给每个模块加一臂。数组落地时 `__typeof` 和 `__truthy`
的 Array 臂正是这样被加进去的，**每个程序多付 11 字节**，包括 `return 1;`。
门控的先例是 `convert::JSON_SET`，谓词**精确**：名字出现。数组的精确谓词是
「有 ArrayLiteral 节点，或程序提到 `JSON`」——后者不能省，因为 `JSON.parse` 能从
编译器看不见的文本里造出数组。

同一格还咬过第二次：`__len` 在这里有身体、却没有任何调用点，**每个模块白背 19 字节**，
直到 `"ab".length` 落地才被发现。**无条件的集合应当定期扫一遍死成员。**

**二、门装错了层。** 方法落地时，门第一版装在**整个集合**上，于是只写 `trim()` 的程序
要为 `indexOf` 付 307 字节。门该装在**每件家具**上，不是房门上。

**三、家具放进了别的房间——这是最贵的一种。** `a.map(f)` 的循环最初内联在**房间 ①**
的每个调用点上，因为大家都以为「房间 ③ 的家具够不到房间 ⑥ 的函数表」。
**那条前提是错的**：把统一签名的 type index 交给 prefab 层就行。循环搬回房间 ③ 之后，
每调用点从 162 字节降到 48。

第三条的教训超出体积：`research/method-binding/` 那场三选一的判决，**结论本来会反过来**
——一个方案之所以看起来输，只是因为它的家具被放在了错误的房间里。
**放错房间不只是代价问题，它会让你把结构问题误读成方案优劣。**

## 语言路线图的依据：一次需求普查（2026-08-28）

**在这一节之前，语言层是供给驱动的**——路线图的下一项是「ECMA-262 目录里还没勾的下一格」。
模板字面量、箭头函数、`"ab".length`、五个方法，都是这么选出来的。每一项都有 design note、
有零成本门实测、有 CLI 跑通，**但没有一项是某个真实脚本要不到才做的**。

同日在下游 agenterm 做的三次需求测量（方法 / GC / `eval`）全部返回**零**。
于是补了第四个测量，这次不是问「有没有人要」，而是问**「现有的脚本在用什么」**：
下游 `scripts/rh/` 有 **82 个生产脚本**，逐条数它们用到的语言构造。

| 构造 | 用它的脚本 | 占比 | 本编译器 | 实测诊断 |
|------|-----------|------|---------|----------|
| `x.method()` | 73 | **89%** | 只有 5 个（trim/indexOf/push/pop/map） | — |
| `for … of` | 64 | **78%** | **没有** | ⚠️ **指错了地方**，见下 |
| 对象字面量 | 61 | 74% | ✅ | — |
| 闭包 / lambda | 58 | 71% | ✅ | — |
| 数组字面量 | 48 | 59% | ✅ | — |
| 模板字面量 | 46 | 56% | ✅ 2026-08-28 | — |
| `import … as` | 38 | **46%** | **没有** | ✅ 诚实点名 |
| `try` / `catch` | 22 | 27% | ✅ | — |
| `switch` | 0 | 0% | 没有 | — 也没人要 |

复跑命令写在 `research/` 之外没有意义，所以直接写在这里：
`grep -rlE '<pattern>' scripts/rh --include='*.rh' | wc -l`，模式见上表行名。
统计的是**用到该构造的文件数**，不是出现次数——问的是「多少脚本会被挡住」。

**这张表推翻了排期。** 两个最大的缺口 `for-of`（78%）与 `import`（46%）
**从来没上过路线图**，而 2026-08-28 一整天做的四项里，最高的一项（模板 56%）
排在第六。不是那四项做错了，是**选它们的依据错了**：目录顺序不是需求顺序。

**`switch` 是这张表最有说服力的一格。** 它在 ECMA-262 里是个显眼的语句，
按目录驱动迟早会做；按需求驱动，**82 个脚本零使用**，它应当一直排在最后。

### 排队时撞见的一条更大的：循环不给每轮新绑定

准备把 `for-of` desugar 成三段式 `for` 时，先去实测三段式 `for` 的闭包语义
（因为 desugar 会原样继承它）。结果不是「可以继承」：

```js
const fs = [];
for (let i = 0; i < 3; i = i + 1) {
  const f = function () { return i; };
  fs.push(f);
}
return "" + fs[0]() + fs[1]() + fs[2]();
```

本引擎答 **`333`**。ECMA-262 13.7.4.7 的 `CreatePerIterationEnvironment` 要求
**`012`**——`let` 声明的循环变量每轮是一个**新绑定**，闭包捕获的是那一轮的那个。
本引擎的闭包**按绑定捕获**，而循环变量整个循环只有一个绑定，于是三个闭包看到同一个槽。

**这条从未被记录过**，而且它就落在能力树里标着 `[x]` 的那一行下面
（`closures that capture an outer local [x] by binding, gated`）。那一行的证据是真的，
但它证的是**工厂函数**造出的闭包（「two instances, two environments」），
**不是循环造出的闭包**——而后者才是真实代码里的常见形状。
`[x]` 在本仓的含义是「已有可执行证据」，这里存在的证据没有覆盖被声称的范围。

**定价（上界）**：下游 82 个脚本里，**29 个（35%）**在循环体内建 lambda。
这是**上界**不是命中数——当场调用、活不过本轮的 lambda 观察不到这条分歧。
统计方法：花括号深度扫描，先剥掉字符串字面量与 `//` 注释再数括号
（先做的一版没做嵌套判断，报了 53，高估 1.8×，此处更正）。

**它改变了排队顺序。** `for-of` 原本排第一，现在排在这条后面，理由是：

1. 这是**正确性**问题，不是覆盖面问题。少一个特性会明确报错；这一条**安静地给错答案**，
   而 `333` 和 `012` 都是合法输出，没有任何诊断会响。
2. `for-of` 会**继承并放大**它。`for (const x of xs)` 的 `x` 在规范里同样是每轮新绑定；
   先做 `for-of` 等于把同一条分歧再铺一层，然后要修两处。
3. 先修它，`for-of` 的 desugar 就自动是对的——这是**顺序省工**，不是顺序洁癖。

**状态：已立项未开工。** 未判定的是实现形状（每轮复制绑定槽 vs 每轮新环境），
以及「不用闭包的循环要不要为此付字节」——按本仓纪律，答案必须是**零**，
所以这条要带门控实测，判据与模板/箭头/方法三次相同：拿**改动前那个提交**当基线。

### 顺带查出的一条缺陷：`for-of` 的诊断指错了地方

```
$ agenterm cli script run forof.qjs      # for (const x of [1,2,3]) { … }
compiling .qjs: this engine needs a value for the `const` binding `x`;
a `const` can never be assigned one later (at byte 22)
```

解析器把 `for (const x of …)` 当成了普通 `const` 声明，于是抱怨缺初值。
**读者被指向 `const`，而真正的缺口是 `for-of`。** 这违反纪律行里那条
「诚实的『尚不支持』诊断（指语法，不指用户）」——`import` 那条就是正例
（`does not support the `import` keyword yet`）。修 `for-of` 会顺带消掉它；
在那之前这条记在这里，不许当成 `const` 的问题去查。

**状态**：`for-of` 是本表排出的下一个语言里程碑（需求驱动，不是目录驱动），
但它**排在上面那条循环绑定分歧之后**——理由见那一节的三条。
`import`（模块系统）是第二位，且它不是一个语法特性而是**一个链接模型**，
须单独立项——很可能要判决性实验，因为「多文件」会改变 `.qjs → .wasm` 这一段
是不是还叫「编译一次」。

## 原生降级（AOT / JIT）：一个被重新提出的方向，尚未立项

**产品定义至今是排除的**：「对齐 JVM（校验+上限+同语义），不对齐 V8（JIT+完整 JS）」，
能力树里 `JIT native code` 与 `device-side AOT` 都是 `[–]`。

**2026-08-28 政委重新提出**：桌面端未来仍会要 JIT/AOT 优化。同日在 agenterm 侧实测，
同一份 wasm、同一个答案、纯计算 2000 万轮：**wasmtime 30.1 ms，tinyvm 16.08 s，535×**；
交叉点约 **1500 轮**真实计算（tinyvm 启动便宜、线性；wasmtime 几乎平）。
`agenterm-wasmcore`（wasmtime）同日按需求归档，所以那条 JIT 路径**不再存在**——
要它就得从本仓这条自研线上长出来。

**四条在立项前必须写清楚的**：

1. **应当先做 AOT，不是 JIT。** 下游 `qualify` 已经是「源码 → 自足产物 + 收据」的
   AOT 形状，接一段原生降级严丝合缝；JIT 要运行期代码缓存、W^X 页与预热，
   是产品现在没有的生命周期。**且 iOS 上 JIT 不可能、AOT 可以**——
   AOT 一套故事通吃，JIT 会把产品劈成两套，而本仓的目标正是 iOS 基座。
2. **真正的未知数是「预算能不能活下来」，不是「能不能发机器码」。**
   本核相对 JIT 引擎的全部优势就是 `steps / depth / pages / slots` 与**确定性收据**。
   原生代码必须把计步**插桩**进去；做不到，收据失去意义、沙箱失去上界，
   那就等于把刚刚归档掉的那个引擎的缺点买回来。**这该立判决性实验**
   （`.claude/skills/decisive-experiment`），判据就是「插桩后的步数是否仍然确定、
   代价是多少」——不是一张功能票。
3. **规模要诚实**：原生降级 = 每个 ISA 一个后端（x86-64、aarch64）。
   这会是本板上最大的一项。
4. **先数需求。** 535× 是拿**纯计算循环**量的；agenterm 的真实脚本是 **I/O 形状**
   （过 `agenterm.*` 门），不是计算形状。2026-08-28 那一轮已经三次
   「假设有需求、实测为零」（方法、GC、`eval`）。**先找出一个真的跨过 1500 轮的载荷**，
   再谈后端。

**状态：候选（未立项）。** 按 `decisive-experiment` §6 的资格，
候选**不得写结论**——上面每一条都是待验证的输入，不是判决。

## Capability tree

Legend: `[x]` 已有可执行证据 · `[~]` 部分完成 · `[ ]` 规划 · `[–]` 有意排除

```text
tinyvm (35)                                      [~]
│
├── product boundary                                     [~]
│   ├── eval_wasm(data, globals, locals)                  [x]
│   │   └── eval / eval_with aliases                      [x]
│   ├── language skin (tinyvm-qjs)               [x]
│   │   ├── qjs2wasm names / ops / host-call subset       [x]
│   │   ├── eval_qjs = eval_wasm(qjs2wasm, globals, locals) [x]
│   │   ├── commissar demo (example commissar)            [x]
│   │   ├── V1 values across the call boundary            [x]
│   │   ├── declarations, functions, control flow         [x]
│   │   ├── object literals, property access, assignment [x]
│   │   ├── functions as values, stored/passed/called     [x]
│   │   ├── Number<->String conversion, per ECMA-262      [x]
│   │   ├── conditional expressions, try/catch/finally    [x]
│   │   ├── JSON.parse / JSON.stringify, per ECMA-262 25.5 [x]
│   │   ├── arrays: the eighth tag, a dense vector        [x]
│   │   │   ├── literal, a[i], .length, nesting           [x]
│   │   │   ├── out of range reads undefined, not a fault [x]
│   │   │   ├── a write past the end fills, no holes      [x]
│   │   │   ├── JSON reads and writes one                 [x]
│   │   │   ├── a string key is not an index (10.4.2.1)   [~] recorded divergence
│   │   │   ├── a non-index property write traps          [~] nowhere to put it
│   │   │   └── methods: push / pop / map                 [x] see below
│   │   ├── an array-free program pays nothing for arrays [x] 9 784 -> 9 784 bytes
│   │   ├── an indexed read is 36.6x the object spelling  [x] 526 vs 19 235 steps
│   │   ├── closures that capture an outer local          [x] by binding, gated
│   │   │   ├── a write after the closure exists is seen   [x] not by value
│   │   │   ├── parameters count; any nesting depth        [x] flat closures
│   │   │   ├── two instances, two environments            [x] identity, observable
│   │   │   ├── a no-capture program pays nothing          [x] 21 fixed / 99 each
│   │   │   └── 一个声明执行两次 = 两个绑定 (14.3.1)        [~] 见下三行
│   │   │       ├── 函数内的 let / const（含 while / 嵌套块） [x] 012，cell 移到声明处
│   │   │       ├── 脚本层的 let / const                     [x] 012，循环内的改走捕获
│   │   │       │   └── 不在循环里的脚本绑定仍是 global       [x] 判据 ④：不许为它涨字节
│   │   │       └── `for` 头部的 let 每轮复制 (13.7.4.7)      [ ] 仍 333；`while` 的 333 是对的
│   │   ├── the whole DecimalLiteral grammar (12.9.3)      [x]
│   │   │   ├── 1.5 · .5 · 1. · 1e3 · 2E2 · 1.5e-3         [x]
│   │   │   ├── integers past i32 and past 2^53            [x] nearest double
│   │   │   └── hex / octal / binary / separators          [ ] own grammars
│   │   ├── template literals (13.2.8)                     [x] folded to `+`
│   │   │   ├── nesting; any expression in a substitution  [x] brace-depth stack
│   │   │   ├── TV normalises CRLF and lone CR to one LF   [x] 12.9.6
│   │   │   ├── a template-free program pays nothing       [x] byte-identical
│   │   │   └── tagged templates                           [ ] needs a raw array
│   │   ├── arrow functions (15.3)                         [x] = a function expression
│   │   │   ├── both parameter forms, both body forms      [x]
│   │   │   ├── the cover grammar, settled before parsing  [x] 13.2.2
│   │   │   ├── an arrow-free program pays no bytes        [x] and no compile time
│   │   │   └── **the equivalence is conditional**          [~] expires if `this` lands
│   │   ├── `"ab".length`                                  [x] UTF-16 code units
│   │   │   ├── counts units, not UTF-8 bytes              [x] café is 4
│   │   │   ├── every other String property still traps    [x] deliberate
│   │   │   └── a program without `.length` got smaller    [x] -19 bytes
│   │   ├── methods: trim indexOf push pop map             [x] binding **measured**
│   │   │   ├── the mechanism was decided by experiment    [x] research/method-binding
│   │   │   ├── trim covers all of Zs + LineTerminator     [x] 12.2 + 12.3
│   │   │   ├── indexOf positions agree with .length       [x] UTF-16 units
│   │   │   ├── map calls back into a function value       [x] a prefab **can**
│   │   │   ├── a plain object's same-named property wins  [x] run-time receiver
│   │   │   ├── adding a method costs non-callers nothing  [x] per-method gate
│   │   │   └── every other method                          [ ] one row + one body each
│   │   ├── host calls with declared raw signatures       [x]
│   │   │   └── a host length answer must be a length    [x]
│   │   ├── nesting bounded by a diagnostic, not an abort [x]
│   │   ├── every rejection names the engine boundary     [x]
│   │   ├── an exhausted heap is legible, not a bare trap [x]
│   │   ├── an uncaught throw is legible, not a bare trap [x]
│   │   ├── the acceptance library runs through a host door [x]
│   │   └── full JS engine / AOT                          [–]
│   ├── host                                              [x]
│   ├── <100KiB>                                          [x]
│   └── iOS runtime boundary                              [x]
│       ├── interpret wasm                                [x]
│       ├── JIT native code                               [–]
│       ├── device-side AOT                               [–]
│       ├── dyn native loading                            [–]
│       └── H5/JS/WKWebView                               [–]
│
├── tinyvm engine                                        [x]
│   ├── scalar WebAssembly                                [~]
│   │   ├── control                                       [x]
│   │   ├── parametric                                    [x]
│   │   ├── locals                                        [x]
│   │   ├── memory                                        [x]
│   │   ├── i32                                           [x]
│   │   ├── i64                                           [x]
│   │   ├── f32                                           [x]
│   │   ├── f64                                           [x]
│   │   └── conv                                          [x]
│   ├── standard proposal profile                        [~]
│   │   ├── bulk memory copy/fill                         [x]
│   │   ├── bulk memory passive lifecycle                 [x]
│   │   ├── sign extension proposal                       [x]
│   │   ├── nontrapping float-to-int                      [x]
│   │   ├── multi-value proposal                          [x]
│   │   ├── single-table funcref profile                  [x]
│   │   ├── multiple defined funcref tables               [x]
│   │   ├── multiple internally defined memories          [x]
│   │   ├── extended constant expressions                 [x]
│   │   ├── tail-call proposal                            [x]
│   │   ├── SIMD proposal                                 [~]
│   │   │   ├── byte shuffle/swizzle                      [x]
│   │   │   ├── integer lane comparison masks             [x]
│   │   │   ├── integer all_true/bitmask reductions       [x]
│   │   │   ├── integer lane min/max + unsigned average   [x]
│   │   │   └── narrow-lane saturating add/sub            [x]
│   │   ├── typed function references                     [ ]
│   │   ├── GC proposal                                   [ ]
│   │   ├── memory64 proposal                             [ ]
│   │   ├── exception handling                            [ ]
│   │   └── threads/shared memory                         [ ]
│   ├── strict load gate                                 [~]
│   │   ├── decode complexity budget                      [x]
│   │   ├── strict declared-memory semantics              [x]
│   │   ├── strict memarg alignment                       [x]
│   │   ├── canonical function expression structure       [x]
│   │   ├── strict i64 signed-LEB range                   [x]
│   │   ├── valid custom-section names                    [x]
│   │   ├── empty memory-section vector                   [x]
│   │   ├── mutable global.set target                     [x]
│   │   ├── strict untyped select value domain             [x]
│   │   ├── export-declared ref.func                       [x]
│   │   ├── imported-global element expressions            [x]
│   │   ├── WABT load-gate oracle                         [x]
│   │   └── static module validation CLI                  [x]
│   └── bounded execution                                [~]
│       ├── persistent instance                           [x]
│       ├── start once                                    [x]
│       ├── per-call fuel                                 [x]
│       ├── explicit guest call stack                     [x]
│       │   ├── host-owned call-depth ceiling             [x]
│       │   ├── host-owned activation-slot ceiling        [x]
│       │   ├── fallible execution-stack growth           [x]
│       │   └── one trap message per ceiling              [x]
│       ├── memory budget                                 [x]
│       ├── table budget                                  [x]
│       ├── deterministic execution stats                 [x]
│       │   └── call/activation peak telemetry            [x]
│       └── in-guest throughput gate                      [x]
│           ├── ns per guest instruction, eight shapes    [x]
│           ├── WABT agrees before any timing is believed [x]
│           └── load-time lowering / stack-top caching    [ ]
│
├── standard host and linking                            [~]
│   ├── owned host ABI                                   [~]
│   │   ├── typed standard function imports               [x]
│   │   ├── bounded in-place host dispatch                [x]
│   │   ├── indexed guest-memory callback context          [x]
│   │   ├── domain + generation guest resource handles     [x]
│   │   ├── bounds-checked guest memory windows            [x]
│   │   ├── two-pass variable-length host result           [x]
│   │   └── string-free fault classification               [x]
│   │       └── every allocator refusal reads Allocation  [x]
│   ├── standard resource imports                        [~]
│   │   ├── standard imported globals                     [x]
│   │   ├── standard imported linear memories             [x]
│   │   └── store-owned imported funcref tables           [x]
│   ├── standard resource exports                        [~]
│   │   ├── linked exported globals                       [x]
│   │   ├── linked exported memories                      [x]
│   │   └── linked exported tables                        [x]
│   └── cross-instance functions                         [~]
│       └── linked exported functions                     [x]
│           ├── numeric value signatures                  [x]
│           ├── store-owned funcref values                [x]
│           ├── opaque externref function/global values   [x]
│           └── standard externref tables                 [x]
│
├── game runtime                                         [x]
│   └── game ABI                                         [x]
│       ├── standard .wasm cartridge                      [x]
│       ├── manifest compatibility                        [x]
│       ├── core v1 imports                               [x]
│       ├── init/tick/suspend/resume                      [x]
│       ├── portable state snapshot                       [x]
│       ├── bounded frame output                          [x]
│       │   ├── recyclable host buffers                   [x]
│       │   └── stable two-pass copy lengths              [x]
│       ├── native module registry                        [x]
│       │   └── atomic resource-table factory             [x]
│       ├── App Store bundled-only gate                   [x]
│       └── machine host profile                          [x]
│           ├── catalog profile binding                   [x]
│           ├── exact zero-budget channel semantics       [x]
│           ├── profile-bound descriptor return           [x]
│           ├── typed compatibility issue report          [x]
│           └── exact-build Wasm feature negotiation      [x]
│
├── native I/O surface                                  [~]
│   ├── indexed2d presentation                           [~]
│   │   ├── bounded app metadata hot path                 [x]
│   │   ├── scoped immutable frame views                  [x]
│   │   └── single-buffer RGBA expansion                  [x]
│   ├── grid3d presentation                              [x]
│   │   └── allocation-free typed cell iteration          [x]
│   ├── tones playback                                   [~]
│   │   ├── bounded PCM tone synthesis                    [x]
│   │   ├── interruption / route / reset owner             [x]
│   │   ├── real App exact tone-event consumption          [x]
│   │   ├── single-buffer WAV + bounded wave LRU            [x]
│   │   └── physical speaker / headphone evidence          [ ]
│   ├── touch/keyboard/controller input                  [~]
│   │   ├── bounded multi-source aggregation              [x]
│   │   ├── Apple keyboard/gamepad adapter                [x]
│   │   ├── overlapping keyboard alias retention          [x]
│   │   ├── real App rising-edge input behavior           [x]
│   │   └── physical keyboard/gamepad evidence            [ ]
│   ├── scene lifecycle + persistence                    [~]
│   │   ├── real App shared session + frame pacer          [x]
│   │   └── protected prepublication snapshot replace     [x]
│   │       └── bounded prepared slot + borrowed restore slice [x]
│   └── physical-device lifecycle/audio/play             [ ]
│
├── evidence                                             [~]
│   ├── Depth Well grid3d                                 [x]
│   ├── Paddle Guard indexed2d                            [x]
│   ├── Signal Lock Swift-to-Wasm migration               [x]
│   ├── deterministic replay vectors                      [x]
│   ├── development JSC + H5 differential                [x]
│   ├── real iOS app consumer                            [x]
│   ├── TestFlight bundled-only candidate                [~]
│   │   ├── exact current-main upload                     [~]
│   │   ├── installed distribution identity verifier     [~]
│   │   └── Apple processing + physical install           [ ]
│   └── physical-device play                             [ ]
│
├── roadmap                                              [~]
│   ├── P0 — close product evidence                      [~]
│   │   ├── TestFlight processing + install               [ ]
│   │   ├── physical device lifecycle                     [ ]
│   │   ├── physical frame-time/resource evidence         [ ]
│   │   └── physical audio-session evidence               [ ]
│   ├── P1 — reusable native modules                     [~]
│   │   ├── generalize memory-zero call-scoped borrowing   [x]
│   │   ├── explicit selected-memory callback context      [x]
│   │   ├── unified host/guest handle lifetimes             [x]
│   │   ├── cross-runtime non-reused table domains          [x]
│   │   ├── native-resource snapshot quiescence             [x]
│   │   ├── versioned native import conventions           [x]
│   │   ├── converter-visible compatibility reports       [x]
│   │   └── platform-neutral host architecture            [~]
│   │       ├── contract / abstraction / backend split     [x]
│   │       ├── internal handles + preopen-only paths       [x]
│   │       ├── optional WASI Preview 1 adapter             [x]
│   │       │   ├── args + environ                           [x]
│   │       │   ├── clock + random                           [x]
│   │       │   ├── preopen discovery + fd_close             [x]
│   │       │   ├── fd read/write/seek/stat                   [x]
│   │       │   ├── path open/unlink                          [x]
│   │       │   └── proc_exit                                 [x]
│   │       └── platform backends outside VM                [~]
│   │           ├── capability-directory std backend         [x]
│   │           ├── iOS Simulator App container wiring        [x]
│   │           └── physical iPhone container evidence        [ ]
│   ├── P2 — accepted standard Wasm coverage             [x]
│   │   ├── proposal priority by real cartridge workload  [x]
│   │   ├── optional SIMD game-kernel subset               [x]
│   │   │   ├── narrow-lane saturating add/sub              [x]
│   │   │   ├── whole-vector bitwise masks                  [x]
│   │   │   ├── signed/unsigned integer comparison masks     [x]
│   │   │   ├── integer all_true/bitmask reductions          [x]
│   │   │   ├── integer lane min/max + unsigned average      [x]
│   │   │   ├── wrapping integer lane arithmetic            [x]
│   │   │   └── scalar/vector lane bridge                    [x]
│   │   ├── independent WABT/JSC differential per leaf    [x]
│   │   └── size/resource budget retained per leaf        [x]
│   ├── P3 — cartridge authoring ecosystem               [~]
│   │   ├── converter/conformance ecosystem               [~]
│   │   │   ├── versioned JSON host-compatibility report   [x]
│   │   │   ├── versioned JSON lifecycle conformance report [x]
│   │   │   └── versioned JSON replay conformance report    [x]
│   │   ├── representative replay publication gate          [x]
│   │   ├── fan-authored standard .wasm                   [x]
│   │   │   └── header-only C core v1 declarations        [x]
│   │   └── external distribution after Apple approval    [ ]
│   └── research queue                                   [~]
│       ├── QJWasm ownership + low-copy lessons           [~]
│       ├── unified host/guest handle lifetimes            [x]
│       ├── bounded call/callback/completion channels      [x]
│       ├── event-loop-neutral async completion ABI        [x]
│       │   ├── owner-thread completion queue core           [x]
│       │   ├── versioned guest import protocol              [x]
│       │   ├── C ABI channel ownership + late delivery      [x]
│       │   ├── Swift MainActor owner + host profile         [x]
│       │   ├── standard async cartridge fixture             [x]
│       │   └── booted iOS Simulator completion lifecycle    [x]
│       ├── cross-boundary copy/call benchmarks            [x]
│       └── JavaScriptCore remains development oracle     [~]
│
├── slot-B                                               [ ]
│   └── dynamic native execution                         [–]
│
└── non-goals
    ├── cu                                                 [x]
    ├── dyn                                                [–]
    ├── chassis                                            [x]
    ├── #78                                                [x]
    ├── WASI as implicit/default game host                 [x]
    ├── APE                                                [x]
    ├── WAT                                                [x]
    └── full JS engine in tinyvm-qjs              [–]
```

The first tree is executable product truth: every `[x]` leaf is backed by an owning
integration test or a three-edge golden family. `[~]` means the branch has useful
implementation but still lacks one or more stated acceptance gates; it must not be read
as “almost approved” or “safe to ship externally.”

## Product boundaries and invariants

### 1. Standard Wasm is the executable format

- The product face is `eval_wasm(data, globals, locals)`: `data` is a standard
  WebAssembly binary module (`\0asm`). `globals` bind the import table at the host
  door; `locals` are this call's arguments. `eval` / `eval_with` remain empty-gate
  aliases. WAT is a development source format, not a runtime input.
- `tinyvm-qjs` is the language layer: a staged `.qjs` -> `.wasm` compiler
  (lex -> parse -> AST -> IR -> encode) plus the skin that runs what it emits.
  `compile_qjs` is compile-only and generic — it knows nothing about any
  embedder's host door — and returns a `CompileError` naming the capability
  boundary and its byte offset. `qjs2wasm` is that compiler under
  `Names::HostImport`, where a bare name is a zero-argument `js.<name>` import;
  `eval_qjs` is `eval_wasm(&qjs2wasm(src)?, globals, locals)`, and its world is
  only those two bindings. The commissar demo is
  `cargo run -p tinyvm-qjs --example commissar`.
- `compile_qjs_m1` is the milestone above that skin, and the one the language
  grows on: statements, `let`/`const`/`var`, `if`/`while`/`for`, functions with
  parameters and `return`, strings, and the full operator ladder — every value
  of it a V1 pair, `(tag: i32, payload: i64)`, chosen by the measured experiment
  recorded in `design-value-representation-experiment.md`. Numbers are binary64,
  so `1/0` is `Infinity` and `2147483647 + 1` does not wrap. Its `main` takes two
  wasm parameters per JavaScript argument and returns two results; `Value` is the
  host's door across that boundary. `compile_qjs` stays beside it only until its
  `i32`-in/`i32`-out callers move.
- Objects are a sixth tag on that same pair, not a second ABI. A record is a
  flat `[len][cap][entries]` header over `[key][tag][payload]` entries — a
  vector, not a shape table, because the population this milestone exists for
  (`fleet.js`) is twelve namespace tables of twelve *different* shapes with one
  instance each, plus parameter objects built per call and read once, and a
  hidden class only pays when many objects share a shape. The reasoning and the
  three conditions that would overturn it are recorded at `OBJ_HEADER` in
  `runtime.rs`, so the next milestone inherits the argument rather than the
  conclusion. Keys are Strings (`o[1]` and `o["1"]` are one slot), a missing
  property reads `undefined` rather than trapping, property order is insertion
  order, and `===` on two Objects is reference identity — which needed no new
  type test, because the V1 pair's payload comparison already *is* one.
- Functions are a seventh tag on the same pair, and calling one is
  `call_indirect` through the module's own funcref table. That instruction
  matches the callee's signature exactly and JavaScript's calls match nothing,
  so the table holds one **adapter** per function that became a value, all of
  one uniform signature, rather than the user's functions. The payload is a
  guest pointer to a one-word record holding the element index — *not* the
  index itself, which is what the first attempt was and what made two
  evaluations of one function expression one function (ECMA-262 15.2.5 makes
  them two). Identity then comes out of the allocator, so `===` needed no new
  type test for the same reason Object needed none.
- The three ECMA-262 conversions between Numbers and Strings are in:
  Number::toString (6.1.6.1.20, shortest round-tripping, Dragon4 in Burger &
  Dybvig's formulation), StringToNumber (7.1.4.1, the whole grammar, correctly
  rounded) and String relational comparison (7.2.13, by UTF-16 code unit rather
  than by byte or code point). So `"a" + 1` is `"a1"`, `"1" - 1` is `0`,
  `"a" < "b"` is `true`, and every Number names a property key and not only the
  integers. The conversion still missing is 7.1.1 ToPrimitive, which needs the
  `valueOf`/`toString` a prototype would carry, so `"" + {}` traps. They cost
  6 625 bytes of emitted wasm in **every** module — an empty script went 2 620
  to 9 771 — carried unconditionally like the rest of the runtime prelude, and
  the lever if that ever matters is per-algorithm gating.
- `throw` and `try`/`catch`/`finally` are in, and **not** as wasm exception
  handling: `crates/tinyvm/src/wasm.rs` has no arm for `try`, `catch`, `throw`,
  `rethrow` or `try_table`, and refuses the tag section, so the capability tree
  still reads `exception handling [ ]`. A throw is a flag plus three module
  globals holding the thrown value as an ordinary V1 pair, and a two-instruction
  check after every call that could raise one. The two designs that lost are at
  `emit::m1`'s `Unwind`: a sentinel value would be an eighth *type*, priced by
  the measured growth law at one more test at every dispatch site and paid by
  every program whether or not it throws — and a completion record is not a
  language value; a table of handler continuations needs a computed jump, which
  rewrites the non-throwing path to buy the throwing one. A program with no
  `throw` and no `JSON` pays nothing at all; one that has them pays four bytes
  per call site. The channel belongs to **one call**: the globals are instance
  state, so the entry prologue clears the flag beside the fault word, without
  which one uncaught throw poisoned a persistent instance for its lifetime.
  An uncaught throw is a third thing at the fault word — neither a budget to
  raise nor a defect to report — and `GuestFault::UncaughtThrow` is how a host
  reads it.
- `JSON` (ECMA-262 25.5) is **an object holding two function values**, not a
  compiler intrinsic: `__json_ns` calls the same `__obj_new` / `__fn_new` /
  `__obj_set` a script writing the object literal would reach, and
  `JSON.parse(t)` is a property read and a call through the value it finds. It
  is the one name this engine binds itself, and one name is not a global scope
  — resolution walks the source's own scopes first, so a script's `const JSON`
  shadows it and an embedder's declaration of the name wins. The set is gated
  on the name appearing, because that predicate is exact where "contains an
  addition" is not, so a program that never writes `JSON` is byte-identical to
  what it was. Naming it also turns the unwind channel on, because
  `JSON.parse` raises one; that is why the two arrived together.
- 下游验收目标 `agenterm/scripts/qjs/lib/fleet.js`：**编译与运行是两条不同强度的主张，
  分开说。**（本条 2026-08-25 由独立复核收紧：原文写作「原样整篇编译并运行」，而它自己
  两句之后就写了「embedder 要自带五行前奏」——既要前奏就不是原样，两句不能同真。）

  | 主张 | 成立吗 | 条件 |
  |------|--------|------|
  | 原样整篇**编译** | ✅ | 仅在 `Names::HostImport` 下。6 280 源字节 → 20 935 wasm，过装载门，29 个函数值属性可达 |
  | 产物**可被宿主喂饱** | ❌ | 它只 import 一样东西：`js.__host`。宿主得回一个带函数属性的客人对象，而门只说原始 i32/f64/字节，`Value` 没有 Object 变体 |
  | 在**产品实际用的** `Names::Declared` 下编译 | ❌ | 停在第 682 字节：`no host function named __host` |
  | 原样整篇**运行** | ❌ | 需 embedder 前置五行 `const __host = {...}`，即非原样 |

  以前停在第 14 行第 727 字节，所以进展是真的——但真正可交付的形状是**按门的实际表面写
  绑定**：`agenterm/scripts/qjs/lib/fleet.qjs` 在 `Names::Declared` 下直接编过
  （3 966 → 12 442 字节，过装载门，import 恰是门提供的 `fleet_call` / `fleet_result`），
  不需要任何前奏。`fleet.js` 需要前奏不是引擎的缺口，是它当初写给了另一套宿主表面。

  跑通的证据不是"编译过了"，而是
  `crates/tinyvm-qjs/tests/fleet_acceptance.rs`：一个 wrapper 经声明的原始宿主门出去，
  broker 用 JSON 文本回答，`JSON.parse` 把它变成对象，调用方读出属性。剩下的一处缩减
  写在那份测试的文件头：`fleet.js` 用 `__host.fleet_call(...)` 触门，那是自由名上的属性
  调用，而宿主答不出对象（`Value` 没有 Object 变体），所以 embedder 要自带五行
  `const __host = {...}` 前奏。补齐它需要"可声明对象形状的宿主命名空间"，那是宿主边界的
  决定，不是这个库的。
- The growth law the value-representation experiment measured — each additional
  type costs one type test per dispatch site — is now being paid, so dispatch
  order is a decision and not an accident. Every Object arm is appended last in
  `__typeof`, `__truthy` and `__to_number`, so no type that existed before
  objects pays for them; Function cost exactly the same, one arm appended last
  in each of those three and nothing anywhere else. The two sites that depart
  from Number-first (`__obj_get`/`__obj_set` test Object first, `__to_string`
  tests String first) say so where they depart, and the site a function value
  runs hot — the call — is not a ladder at all but a single tag test.
- A compiled `.qjs` reaches a host capability *with arguments* through
  `Names::Declared`: the embedder declares raw wasm functions — module, field,
  signature — and how each JavaScript argument maps onto their parameters, and
  the compiler unwraps. A String argument becomes `(ptr, len)` into linear
  memory; a variable-length byte result becomes a String again through a
  two-pass read onto the guest's own bump heap — whose announced length is
  checked to *be* a length before it becomes a size, because comparing the
  copied count against the announced one only compares two host answers to each
  other, and `-1` twice (a raw contract's "your buffer is too small") passed
  that check while producing a String with a fabricated 4 GiB tail and walking
  the bump pointer backwards over the fault word. **The door stays raw.** It never
  learns what a JavaScript value is, so the same host stands behind a
  hand-written `.wasm` guest and a compiled `.qjs` one, through one import
  table. Making the door speak `(tag, payload)` would break every hand-written
  guest and would leak one language's value representation into a boundary meant
  to serve any guest. `tinyvm-qjs` therefore owns the mechanism and names
  nobody's host function; the vocabulary belongs to the embedder. A wrong
  argument type is a compile diagnostic where the compiler can settle it and a
  trap where only the run can — never a silent coercion.
- 客户机自己的资源耗尽必须和语义错误分得开。`memory.grow` 被拒返回 `-1` 而不是陷阱
  （标准 Wasm；`crates/tinyvm/src/wasm.rs` 的 `Op::MemoryGrow`），拒绝本身不带理由，
  编译出的 bump 分配器只能落进一个普通 `unreachable` —— 和这一里程碑缺失的某个转换执行
  的是同一条指令，宿主拿到的是同一个 `WasmError`、同一句 `unreachable executed`、同一个
  `FaultClass::Guest`。靠"内存到顶了，那就算预算问题"去猜，会把一个真的坏脚本判成预算
  不足，正是分类要避免的那种静默误判。所以客户机在倒下之前把原因写进自己线性内存的第
  一个字（`__alloc` 唯一会写那里的地方，bump 指针永远不会发到那个地址），宿主陷阱之后用
  `tinyvm_qjs::guest_fault(&instance.memory()?)` 读出来：`Some(GuestFault::HeapExhausted)`
  是预算，`None` 是脚本自己的问题。不新增 import、不新增 export、不需要宿主在场，
  代价是每个产物 14 字节。

- Syntax nesting is bounded by a number the compiler keeps, not by the native
  stack. Recursive descent overflows into a process abort, which for a host
  compiling untrusted `.qjs` is the worst failure mode there is — no caller is
  left to hear about it — so reaching the depth budget is an ordinary refusal
  with an ordinary diagnostic.
- The compiler moved *up* into this crate on 2026-08-24, from AgenTerm's
  `agenterm-qjswasm`, on one principle: generic dynamic-engine capability
  belongs in tinyvm, business belongs in the embedder. The 1113 lines that
  moved contain no embedder vocabulary; the host door and slot policy that do
  stayed behind. AgenTerm now consumes `tinyvm-qjs` by git revision instead of
  carrying its own copy.
- **Open, not decided here:** the non-goal tree below still reads `full JS
  engine in tinyvm-qjs [–]`. That line was written when this crate was a demo
  skin. It is now the compiler an embedder's language roadmap runs on, and that
  embedder's own PRD treats JS coverage as scheduling rather than exclusion.
  The two statements cannot both stay true forever. Left standing rather than
  silently flipped: reconciling them is the owner's call, not a migration's.
- TinyArcade metadata lives in a standard custom section. Host capability use remains
  ordinary versioned function imports.
- `tinyvm module validate FILE.wasm` validates an ordinary module without requiring a
  TinyArcade manifest, binding imports, instantiating it or running its start function.
- The VM never invents a private opcode to make a game easier to implement.

### 2. Interpretation is the iOS execution boundary

- Downloaded or bundled `.wasm` is interpreted. The runtime does not generate executable
  pages, device-AOT downloaded code or load a cartridge-supplied dynamic library.
- Build-time compilation from game source to standard `.wasm` is expected and is not the
  excluded device-side AOT path.
- `agenterm-dyn` is a separate desktop native door and is never a tinyvm execution backend.
- JavaScriptCore WebAssembly is a development oracle through `JSContext`, not the shipped
  authority, dependency or fallback.

### 3. Untrusted input is bounded before allocation or mutation

- Cartridge bytes are capped by the embedding; every allocation-amplifying section record
  is additionally charged to one decode-complexity budget before reserve.
- Memory pages, table elements, call depth, aggregate live activation slots and per-call
  instructions are host-owned ceilings.
- Guest-sized vectors use fallible growth. A malformed count must return a typed error,
  never abort the process while attempting an infallible allocation.
- Load-time type/structure errors fail before a module becomes invokable. Runtime traps
  remain local to the affected instance.
- 宿主回调拿到的是裸 `&mut [u8]`，把客户机给的 `(ptr, len)` 变成切片是嵌入方唯一一处
  算错就读写宿主内存的算术。核自带这道门：`guest_window` / `guest_bytes` /
  `guest_bytes_mut` / `guest_str` / `guest_write`。i32 地址按标准 Wasm 当无符号解释，
  越界或求和溢出一律返回 `WasmError`，不夹取、不截断、不 panic；坏指针
  (`guest memory window`)、坏文本 (`guest memory utf-8`) 与客户机缓冲区过小
  (`guest memory window too small`) 各占一句。
- 宿主回调整段持有客户机内存的 `&mut`，无法回身调用客户机导出的分配器；要交回变长结果
  只能两趟：先返回长度，客户机自己分配，再回来取字节。这是本 VM 的结构性约束，不是某个
  embedder 的口味，所以核提供机制本身 `PendingResult`：有上限、fallible 增长、只交付一次；
  目的缓冲区不够是自成一句的条件 (`pending result destination`) 且不丢暂存结果，客户机可以
  重分配再取。核不定义任何 import 名或状态码，那是 embedder 的 ABI。
- 核 fmt-free，`WasmError::message()` 是下游唯一能分类的通道，所以每一种要分开处理的条件
  各占一句 `&'static str`，不共用一个含混词。上限一句一个：`call depth` /
  `activation slot limit` / `operand stack` / `control stack` / `step budget` /
  `memory page limit` / `table element limit`；旁边的分配与记账失败也各自成句：
  `call stack allocation` / `activation slot overflow` / `memory allocation` /
  `memory size overflow` / `memory size accounting` / `table allocation` /
  `table size overflow`。文案表在 `WasmError` 的文档注释里。
- 但下游不该复制那张表：按字符串分类，文案一改就悄悄失配（这已经真实发生过一次）。
  分类由核给：`WasmError::class()` 返回 `FaultClass`（Load / ResourceCeiling /
  Allocation / Guest / Internal），`WasmError::ceiling()` 直接点名是哪一条 `Limits`
  ——`max_steps` / `max_call_depth` / `max_activation_slots` / `max_memory_pages` /
  `max_table_elems`，宿主知道该抬哪个数。`WasmError` 的类型形状不变，没有新变体。
  分类只靠一条命名规则：分配失败的文案一律以 `allocation` 结尾，这条规则由测试扫源码守着。
- 但这条规则得从**分配点**扫，不能从文案扫。原来的守卫只检查"含 `alloc` 的文案必须以
  `allocation` 结尾"——它只复查已经守规矩的那些站点，忘了说 `allocation` 的那些正因为
  忘了说而扫不到。改成从每个 `try_reserve*` 出发跟到它的失败臂之后，一次抓出 24 个站点、
  28 条文案：21 条被 `class()` 判成 `Guest`（宿主内存不够被说成脚本坏了），3 条被判成
  `ResourceCeiling`（`operand stack`——宿主被告知撞上了一条它没设、也抬不了的固定上限）。
  文案全部改成以 `allocation` 结尾；`instance address space` / `function locals` /
  `operand stack` 三条是一词两用，按本仓已有的先例拆开，非分配的那一侧保留原名。
  `WasmError` 的形状不变，没有新变体。

### 4. VM capability and TinyArcade profile are separate

- The general VM supports typed numeric/reference host calls, imported/exported resources,
  multiple memories and tables, and cross-instance linking.
- TinyArcade core/native v1 deliberately stays i32-only, requires exactly one defined memory
  zero, and currently rejects table imports. Those are embedding rules, not VM language limits.
- New standard proposals enter only after decode, validation, execution, resource-budget and
  independent-engine evidence agree. “A reference engine accepted it” is not sufficient by
  itself.

### 5. Host ownership is explicit

- Imported memories, tables, globals and linked functions belong to an explicit `WasmStore`.
  Clone/re-export/sibling binding must preserve one live identity rather than copy snapshots.
- `funcref` stores a store-owned function address; `externref` stores only an opaque,
  process-unique token. Tinyvm never treats a native object address as a guest value.
- The current host callback receives call-scoped parameter/result slices and a zero-copy
  mutable view of memory zero. Future multi-memory context must identify the selected memory,
  remain synchronous and bounded, and be impossible to retain after callback return; it must
  not use `unsafe` merely to bypass borrow checking.
- Native async work uses an event-loop-neutral bounded completion queue. It reserves request
  count and maximum response bytes before work starts, uses runtime-local generation-checked
  tickets, transfers completed payload ownership without a second copy, and must quiesce before
  portable suspend. Platform scheduling and versioned module imports remain outside the VM.
- The iOS C/Swift boundary owns completion channels separately from runtime handles. A channel
  binds to at most one runtime, cannot close while bound, clears tickets when that runtime closes,
  and rejects wrong-thread or late delivery. Host profiles publish the same completion imports.

### 6. App Store distribution is a separate authority gate

- The production Swift package defaults to `appStoreBundledOnly`; external catalog/private
  cartridge APIs are excluded from the shipping app package unless a release explicitly opts
  into an Apple-approved policy.
- A valid signature, manifest or host profile proves artifact identity/compatibility, not
  Apple permission to download and execute external code.
- Official reviewed catalog, private user import and public fan marketplace are distinct
  product paths. None inherits authority from another.

## Current capability summary

| Layer | What exists now | Safe failure |
|---|---|---|
| Module | Standard binary decode, static validation, custom sections, imports/exports and start | Decode/validation error before execution |
| Execution | Persistent instance, explicit activation trampoline, fuel and resource telemetry | Deterministic trap local to the instance |
| Values | i32/i64/f32/f64, funcref and opaque externref through supported standard locations | Exact type mismatch rejection |
| Resources | Defined/imported/exported globals, memories and funcref/externref tables | Binding/type/limit rejection or borrow-conflict trap |
| Host ABI | Typed arbitrary-arity compatibility callback, fixed 16-value hot path, generation-checked guest resources, bounded native completion queue and optional capability-directory std backend | Callback/platform error propagated without ambient-path reconstruction; saturation/oversize/stale completion fails typed, while persisted snapshots must quiesce native resources |
| Optional WASI P1 | Sixteen exact process/clock/random/preopen/descriptor/path/exit imports over the neutral host contract | Unknown/wrong imports fail binding; bad memory, path, rights or backend absence returns explicit errno/interruption |
| TinyArcade | Manifest, core v1, native registry, lifecycle, deterministic RNG/clock, render/audio/state bounds | Cartridge fails closed; App owner remains alive |
| iOS | C ABI, XCFramework, Swift package, input/frame pacing, persistence, replay and native 2D/3D/audio owners | Main-actor owner latches bad runtime and clears stale output |
| Tooling | Validate, attach-manifest, descriptor, host profile, replay and deterministic catalog publisher | No output publication after failed preflight |

## Game-platform contract

```text
standard .wasm cartridge
    → static module validation
    → TinyArcade manifest + import/export compatibility
    → origin / trust / App Store policy
    → bounded persistent instance
    → init
    → repeated input + monotonic clock → tick → render/audio
    → suspend snapshot + host clock
    → resume or discard-and-create-fresh
```

The core namespace is `tinyarcade:core/v1`. Native extensions use separate,
versioned namespaces compiled into the App and registered with exact signatures and
per-lifecycle quotas. A cartridge declaration requests compatibility; it does not grant
itself a native implementation or distribution authority.

Media is discriminated and bounded:

- `tinyarcade:grid3d/v1` — logical 3D grid records for Depth Well–style scenes;
  Swift owns one immutable frame and iterates typed cells without a second array.
- `tinyarcade:indexed2d/v1` — indexed pixels plus palette for classic 2D games.
- `tinyarcade:tones/v1` — bounded sequential tone events and aggregate duration.

Portable replay stores the exact cartridge hash, manifest identity, initial snapshot,
monotonic input/clock facts and per-frame output length/digest. Execution still uses the
original `.wasm`; replay is evidence, not another executable format.

## Roadmap decisions

### P0 — finish evidence on the product that already exists

The next release gate is not another interpreter feature. It is Apple-side TestFlight
processing/installability followed by physical-iPhone lifecycle, frame-time, resource,
input feel and audio-session evidence. Simulator and archive evidence cannot close these
leaves.

### P1 — native modules without a hidden copy tax

The existing synchronous host door already lends memory zero directly, without copying the
whole guest memory. The next reusable design must generalize that fact for standard
multi-memory modules: specify memory selection, immutable/mutable access, re-entry rejection,
growth exclusion, result reservation before callback, C/Swift pointer lifetime and
deterministic failure. QJWasm is useful here for ownership and low-copy lessons; QuickJS, its
unsafe threading model and its JavaScript product dependency are not adopted.

QJWasm also supplies research targets beyond raw memory access: one lifetime model for host
objects and guest handles, explicit call/callback/async-completion channels, event-loop
integration that does not hard-code QuickJS, and benchmarks that expose boundary copies and
dispatch cost. tinyvm will translate those lessons into bounded native-module ABI work; it
will not make JavaScript, QuickJS or a second runtime part of the iOS product dependency.

The host implementation follows three boundaries. The contract is a versioned Wasm import
namespace: TinyArcade remains the game profile, while a separately enabled
`wasi_snapshot_preview1` subset may serve non-game embeddings. A `no_std` abstraction owns
opaque handles, clocks, random filling, byte I/O, exit state and a virtual preopen table; the
guest never sees OS descriptors or physical paths. Platform backends live outside the VM:
Unix may share an implementation with cfg-gated macOS sandbox handling, while Windows and
iOS require separate path/handle/storage policy. Unsupported operations return explicit
`NOSYS`/`NOTCAPABLE`-equivalent errors.

This remains one published `tinyvm` crate. Core, contracts, host abstractions and
optional platform backends may be separate modules and feature gates, but there are no
forked Unix/iOS/Windows VM crates with drifting semantics. The default build stays
`no_std + alloc`; enabling a backend must not alter standard Wasm validation or execution.

`host/` itself is not a standard. Normative inputs are WebAssembly Core imports/exports and
linear memory, WASI Preview 1 when that optional adapter is selected, and—only for a future
component profile—WIT, Canonical ABI and the Component Model. Wasmtime/Wasmi/WAMR/wasm3 and
the Wasm C API are implementation references, not the contract. ISA portability continues
to come from portable guest and host code, never dynamic host machine code.

### P2 — grow standard coverage from workload evidence

Every feature family reported by `WasmModule::feature_usage` now has an authoritative matrix
row connecting a standard fixture, executable WABT/tinyvm/JavaScriptCore differential and a
default or opt-in product-size profile. The signed-PCM SIMD subset additionally runs in H5 as
a development oracle. JavaScriptCore's lack of multiple-memory support is recorded as an
explicit capability boundary rather than weakening the WABT/tinyvm semantic check.

This closes the acceptance workflow for the current reported feature set, not every opcode in
every WebAssembly proposal. Typed function references, GC, memory64, exception handling and
threads remain separately workload-gated proposals. A future family may be reported only after
adding its fixture, independent oracle, executable gate and size profile to the standard-feature
matrix; no decoder-only implementation counts as accepted coverage.

### P3 — authoring and cartridge ecosystem

The converter/conformance path targets standard `.wasm` plus the versioned host profile. A
freestanding C fixture now proves that a non-Rust, non-tinyvm producer can compile, attach the
canonical manifest, execute, snapshot and match tinyvm/JSC/H5 replay output. A usable creator
product and external catalog remain product goals; App Store distribution is enabled only after
a verifiable Apple-approved route exists. Development-time Safari/JSC execution stays useful as
an oracle and never becomes the nostalgia-arcade runtime.

## Acceptance and evidence

The runtime foundation is complete only when all of the following are true:

1. Standard modules are independently validated and every accepted proposal has decode,
   validation, execution, budget and differential evidence.
2. Malformed or hostile modules fail through typed errors without allocator abort, native
   stack overflow, stale output publication or cross-instance corruption.
3. Rust, C and Swift owners agree on lifecycle, lengths, identity and resource accounting.
4. At least two structurally different real games run through the same public ABI and produce
   deterministic replay evidence.
5. The exact current-main runtime is consumed by a real arm64 iOS App target.
6. The TestFlight build is processed, installable and exercised on a physical iPhone across
   foreground/background, interruption, save/restore, input, rendering and audio.

Current evidence owners:

- [Executable goal and incremental evidence](../plan/goal-tinyvm-ios-game-runtime.md)
- [Optional WASI Preview 1 profile](../docs/tinyvm-wasi-preview1.md)
- [Capability-based std host](../docs/tinyvm-std-host.md)
- [Optional iOS WASI host](../docs/tinyvm-ios-wasi-host.md)
- [Selected-memory host callbacks](../docs/tinyvm-selected-memory-host.md)
- [Host resource table](../docs/tinyvm-host-resource-table.md)
- [Native completion channel](../docs/tinyvm-native-completions.md)
- [Optional SIMD audio profile](../docs/tinyvm-simd-audio.md)
- [In-guest interpreter throughput](../docs/tinyvm-interpreter-throughput.md)
- [JavaScriptCore public/private boundary](../docs/tinyarcade-javascriptcore-boundary.md)
- [Converter conformance](../docs/tinyarcade-converter-conformance-v1.md)
- [Cartridge conformance report v1](../docs/tinyarcade-cartridge-conformance-report-v1.md)
- [Host compatibility report v1](../docs/tinyarcade-host-compatibility-report-v1.md)
- [Replay conformance report v1](../docs/tinyarcade-replay-conformance-report-v1.md)
- [C cartridge authoring](../docs/tinyarcade-c-authoring-v1.md)
- [Catalog transport](../docs/tinyarcade-catalog-transport-v1.md)
- [Catalog publisher](../docs/tinyarcade-catalog-publisher-v1.md)
- [QJWasm/WA2X source review](../docs/qjwasm-wa2x-source-review.md)
- [Cross-boundary benchmark](../docs/tinyvm-boundary-benchmark.md)
- [Real-cartridge feature usage](../docs/tinyvm-feature-usage.md)
- [Accepted standard-feature matrix](../docs/tinyvm-standard-feature-matrix.md)
- `crates/tinyvm/tests/` — public Rust black boxes and independent fixtures
- `crates/tinyvm/ios/` — C/Swift package and platform smoke gates

## Explicit non-goals

- H5 mini games, DOM APIs, a WKWebView game shell or JavaScript as the cartridge platform.
- JIT, downloaded native code, cartridge-provided dylibs or device-side AOT on the iOS path.
- WASI as an implicit/default game host surface. An optional, versioned Preview 1 profile may
  be implemented separately; files, network, rendering and storage are never ambient powers.
- WAT parsing in the runtime. Producer tooling may compile WAT to standard `.wasm`.
- Claiming App Review approval from technical validation, TestFlight upload or a release note.
- Replacing `agenterm-dyn`, `agenterm-cu` or `agenterm-chassis`; their product boundaries stay
  separate.
