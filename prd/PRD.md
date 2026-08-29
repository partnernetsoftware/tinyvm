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
    R1["① lex / parse / AST<br/><i>a rejection names the engine, not the author</i><br/><i>templates fold to +, arrows to function<br/>expressions, for…of to an index loop<br/>— none of the three gets a node</i>"]
    R2["② lower to V1<br/>eight tags · dispatch order<br/><i>Number, then String, then the rest</i><br/><b>storage &amp; lifetime live here</b><br/><i>a cell per declaration, not per frame</i>"]
    R3["③ runtime prelude<br/>bump heap · object and array records<br/>method bodies: trim indexOf push pop map<br/>includes startsWith endsWith<br/><i>gated: unused means unemitted</i><br/><i>per method, not per set</i>"]
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

  PRD["📄 prd/PRD.md capability tree<br/><i>canary both ways: a checked leaf must have a test,<br/>an unchecked one must not already have one</i><br/><i>the reverse table is hand-written, not topic-matched —<br/>a refusal test looks exactly like a shipped one</i>"] -.->|"what a leaf claims, a test answers for"| R1
  style PRD fill:#eee,stroke:#999,stroke-dasharray: 5 5,color:#555
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

**四、树自己也会放错，而且有两个方向。** 前向金丝雀（`prd_x_leaves_have_suite_edges`）
防的是 `[x]` 吹牛；2026-08-29 一天抓到四片叶子反过来——测试早就在，叶子还写 `[ ]`。
反向那条（`prd_unchecked_leaves_are_not_already_done`，`f4bf11f`）**不能按主题猜**：
`parse_int_is_not_silently_number` 是一条**拒绝**测试，按主题看和「做了 `parseInt`」
一模一样，所以表是手写的，每片开着的叶子预先登记它做成时会存在的测试名。
**它自己的注释又放错了一次**——说 `push / map` 那两行在 `7c4f9dc^` 时还是 `[ ]`，
其实那只在下游 PRD 里发生过，本文件第 501 行一直是 `[x]`。没有金丝雀核对金丝雀的注释，
这条靠人读出来，还没改。（写这一段时又被它咬了一口：它读**每一个** fence，
不只能力树那个——上面 mermaid 里一个带记号的标签，一分钟内就被前向金丝雀点名了。）

---

**房间 ② 有一种和 ③ 完全不同的放错法：东西放对了房间，但放在了房间里错误的时刻。**

2026-08-28 的每轮绑定分歧就是这一种。捕获绑定的 cell 分配确实属于房间 ②——存储与
生命周期是降级的事，不是解析的事，也不是运行时预制件的事。它没放错房间，
**它放错了时刻**：开在**函数入口**，而 ECMA-262 14.3.1 说绑定属于**声明这条语句**。
一个函数一格，于是循环体三趟写同一格，三个闭包读出同一个值——`222` 而不是 `012`。

这种放错法比 ③ 那三种更难发现，原因写在判据里：**它不报错。**
少一个特性会在房间 ①「大声拒绝」，付错字节会在门控实测里露出来，
而这一条给出的 `333` 与正确的 `012` 都是合法输出，**没有任何一道门会响**。
它是靠「排 `for-of` 的队时顺手实测了一下将要复用的东西」才掉出来的。

同一天它还自己复发了一次，形状一模一样。第二步把脚本绑定改成捕获时，
`place()` 先判的是「这个绑定属于谁」（`func == SCRIPT`），于是**闭包内部**读它
也拿到了全局——读到的正是「最新那个 cell」，也就是要修的那个共享答案。
正确的问法是「**谁在读**」：脚本读全局，嵌套函数读自己的环境。
**一个决定有两个答案时，先问的必须是「谁在问」，不是「这是什么」。**

所以这张图上要补的一格判据，是在「东西放哪间」之外的：
**在这一间的什么时刻，以及为谁。**

---

**图外一课：语料的归属先于任何普查。** 2026-08-29 两次需求普查拿下游 82 个 `.rh` 脚本
当语料排了八个里程碑。同一份数据，一天之内换了三个前提——「本产品的脚本」→
「另一个仓的脚本」→「本产品要迁移过来的脚本」——结论跟着翻了三次，
而数据一个字没变。**每一次翻转都来自一句我问不出、只能等的产品判断。**
所以普查的第一行不该是数字，该是「这批语料是谁的、要拿它做什么」；
答不上来就把两种读法**并排写**，不选一种当唯一结论。

---

**房间 ① 有一种反过来的用法，`for…of` 是第一个例子：把本该在房间 ③ 的东西，
用房间 ① 已有的词汇写出来，于是房间 ③ 一个字节都不用加。**

`for…of` 需要一个运行期检查——「右边到底是不是数组」，编译期答不了。
直觉的做法是给房间 ③ 加一个预制件，再给它配一道门。实际做法是发现
**这个检查可以完全用子集里已有的语句写出来**：`typeof S === "string"`、
`typeof S !== "object"`、`S === null`、`typeof S.length !== "number"`，
四道 `if` + `throw`，全是房间 ① 折出来的普通语句。
于是房间 ③ 没变，零成本门不需要谓词——**没有门要维护，因为没有东西要门控**。

这条和上面三条「放错房间」是同一枚硬币：**先问这件事能不能用已有房间的词汇说出来，
再决定要不要新开一格。** `map` 那次的教训是「够不到」这个前提要实测；
这次的教训是「说不出来」这个前提也要实测。

顺带记一条守卫的**顺序**教训，它不在任何一间房里，在语言本身：
四道守卫第一版只写了最后一道，结果 `for (const x of 42)` 在读 `42 .length` 时
**先 trap 了**（原始值上取属性走 `require_tag` 到 `unreachable`），
连抛都没轮到，`catch` 什么也看不见。**一道守卫必须先把类型收窄到它自己不会炸的范围，
才轮得到它去判断。**

## 语言路线图的依据：一次需求普查（2026-08-28）

> **前提更正（政委 2026-08-29）。** 下面两次普查的语料是下游 `scripts/rh/` 的 82 个
> `.rh` 脚本。**那是另一个产品的语料**：`.rh` 是独立仓 `partnernetsoftware/rh` 的引擎
> 前端，已暂停开发，不归本产品线。本产品线只管 `.qjs` 与 `.wasm`。
>
> 因此：**照普查落地的八个里程碑保留**（模板、箭头、`.length`、方法、每轮绑定、
> `for…of`、模块、字符串方法、`Number`——每个都是通用 JS 能力，有实测与门控），
> **但「按那张表排序」这条依据不成立**，两节的排序结论一律降为「历史记录」。
> `.qjs` 今后的需求要从 `.qjs` **自己的用途**里来——下游清单 A1。
>
> **再更正（同日，政委第二句）：「`.rh` 安排归档，脚本体系转为 `.qjs`」。**
> 于是那 82 个脚本从「别人的语料」变成 **`.qjs` 的迁移语料**——两次普查量的正是
> 迁移要跨过的东西，**排序恰好是对的**。同一份数据，三种前提，三个结论。
> 记在记忆宫殿：**量之前问清「这是谁的语料、要拿它做什么」；问不到就把两种读法都写下来。**

**在这一节之前，语言层是供给驱动的**——路线图的下一项是「ECMA-262 目录里还没勾的下一格」。
模板字面量、箭头函数、`"ab".length`、五个方法，都是这么选出来的。每一项都有 design note、
有零成本门实测、有 CLI 跑通，**但没有一项是某个真实脚本要不到才做的**。

同日在下游 agenterm 做的三次需求测量（方法 / GC / `eval`）全部返回**零**。
于是补了第四个测量，这次不是问「有没有人要」，而是问**「现有的脚本在用什么」**：
下游 `scripts/rh/` 有 **82 个生产脚本**，逐条数它们用到的语言构造。

| 构造 | 用它的脚本 | 占比 | 本编译器 | 实测诊断 |
|------|-----------|------|---------|----------|
| `x.method()` | 73 | **89%** | 只有 5 个（trim/indexOf/push/pop/map） | — |
| `for … of` | 64 | **78%** | ✅ 2026-08-29，折叠成索引循环 | — |
| 对象字面量 | 61 | 74% | ✅ | — |
| 闭包 / lambda | 58 | 71% | ✅ | — |
| 数组字面量 | 48 | 59% | ✅ | — |
| 模板字面量 | 46 | 56% | ✅ 2026-08-28 | — |
| `import … as` | 38 | **46%** | ✅ 2026-08-29，编译期取入 | — |
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

**状态：2026-08-28 已完成，三步全落地。** 事实 1–4 全部变成 `012`，事实 5（`while`）
仍是 `333`（**正确**，判据 ②）。上游 **902 passed / 0 failed**。

代价按判据 ⑤ 记为斜率而不是截距：**每多一个「循环内被捕获的绑定」+83 字节**，
已写成 `what_a_per_iteration_binding_costs_is_written_down`。判据 ③④ 不另测——
它们就是 `closures_m3.rs` 里那几个既有字节期望（9765 / 9886 / 9929 / 15386、
每个捕获函数 99），本里程碑**一个都没动**，那既是测量也是结论。

三步各自的形状：
1. **`let`/`const` 的 cell 挪到声明处** —— 14.3.1 是关于「声明」的规则，
   所以 `while` 体与嵌套块被同一行代码一起修好，不必分别提到。
2. **循环内的脚本绑定改走捕获** —— 减少一个特例而不是加一个。
   `place()` 原先先判 `func == SCRIPT`，于是**闭包内部**读它也读到全局，
   拿到的正是「最后那个 cell」——要修的病自己又犯了一次。
   改成先问**谁在读**：脚本读全局，嵌套函数读自己的环境。
3. **`for` 头部每轮复制** —— 13.7.4.7，插在 **body 之后、update 之前**。
   位置是语义的一部分：放在 body 之前，第 N 轮会拿到第 N−1 轮结束时的值。

一条值得留的更正：`a_write_to_the_loop_variable_in_the_body_reaches_its_closure_and_the_update`
的期望值先写成 `3:024`，实测 `3:135`，**实测是对的**。闭包 push 在 `i = i + 1` 之前，
直觉以为它冻住了旧值；没有——闭包捕获的是**绑定**，body 的写进的是同一个绑定，
复制发生在 body 之后。要同时满足「每轮新绑定」和「仍然按绑定而非按值捕获」才会是 `135`。

### ~~顺带查出的一条缺陷：`for-of` 的诊断指错了地方~~ —— 2026-08-29 随 `for-of` 落地消失

```
$ agenterm cli script run forof.qjs      # for (const x of [1,2,3]) { … }
compiling .qjs: this engine needs a value for the `const` binding `x`;
a `const` can never be assigned one later (at byte 22)
```

解析器把 `for (const x of …)` 当成了普通 `const` 声明，于是抱怨缺初值。
**读者被指向 `const`，而真正的缺口是 `for-of`。** 这违反纪律行里那条
「诚实的『尚不支持』诊断（指语法，不指用户）」——`import` 那条就是正例
（`does not support the `import` keyword yet`）。**已消掉**：`for-of` 的头部现在由三个 token 的前瞻认出来，`declaration` 根本轮不到看它。
`arrays_m3.rs` 里那条写着「this is the upstream test that will notice when it is fixed」
的断言，就是通知这件事的那个哨兵——它响了，然后被翻成了「已修复」。

**状态**：`for-of` 已于 2026-08-29 落地（需求驱动的第一项）。它排在循环绑定分歧之后
才做，理由见那一节的三条——而那个顺序是对的：`for-of` 的元素绑定**一行没写**就是
每轮新的，因为它的声明落在循环体里，前一个里程碑的 `fresh_cell` 直接接住了。
先做 `for-of` 就要把同一件事修两遍。

**`import`（46%）也已于 2026-08-29 落地。** 立项时写的那句「它不是语法特性而是
**链接模型**」**是错的**，更正见 `plan/design-module-milestone.md` §1：
产物仍是一个 `.wasm`，仍是一次编译，装载期仍只有一个模块。

推翻它的证据不在普查表里，在本产品自己身上：下游
`tests/qjs_produces_a_fleet_operation.rs` 里那行 `format!("{}\n{driver}",
fleet_binding())`——使用那个 286 行、29 操作的 `fleet.qjs` 库的**唯一**办法，
是在 Rust 里把库文本拼到脚本前面。那个库零消费者不是「还没人用」，是**用不了**。
`import` 要做的就是**把那个 `format!` 做对**，而「做对」的部分是命名空间。

**这张表现在没有缺口了，而这件事本身要说清楚。** 剩下的行全是 ✅，
所以下一轮的依据**不能再是这张表**。一张全绿的需求表不是「做完了」，
是「**这把尺量不动了**」。换了一把尺，见下。

### 第二次普查（2026-08-29）：换一批构造，排序完全变了

第一张表只数了**语法**构造。同一个 82 脚本语料，改数控制流细节与**标准库面**：

| 构造 | 脚本数 | 占比 | 出现次数 | 本引擎 | JS 里叫什么 |
|------|-------|------|---------|--------|-------------|
| `.contains(` | **58** | **71%** | **721** | ✅ 2026-08-29 | `String.prototype.includes` |
| `.starts_with` / `.ends_with` | **41** | **50%** | 169 | ✅ 2026-08-29 | `startsWith` / `endsWith` |
| `.split(` | 34 | 41% | 129 | ✅ 2026-08-29 | `split` |
| `.to_lower`（`to_upper` **零使用**） | 25 | 30% | 67 | ✅ 2026-08-29 | `toLowerCase` |
| `break` | 17 | 21% | 56 | ✅ 2026-08-29 | 同名 |
| `continue` | 15 | 18% | 48 | ✅ 2026-08-29 | 同名 |
| `.replace(` | 14 | 17% | **142** | ✅ 2026-08-29 | `replace` **与** `replaceAll` |
| ~~spread `...`~~ | **0** | **0%** | **0** | — | **假阳性，见下** |
| `parse_int` / `to_int` | 7 | 9% | 17 | ✅ 2026-08-29 | `Number()`（**不是** `parseInt`） |
| `.len()` | 4 | 5% | 6 | ✅ | `.length` |
| ~~`switch`~~ | **0** | **0%** | **0** | — | **假阳性，见下** |
| 三元 `?:` | 1 | 1% | 1 | ✅ 已有 | 同名 |
| `do { } while` · 解构 · 默认参数 | **0** | 0% | 0 | — | — |

复跑：`grep -rlE '<pattern>' scripts/rh --include='*.rh' | wc -l`（脚本数）与
`grep -rhoE … | wc -l`（次数）。**两个数都记**，因为它们回答不同的问题：
脚本数是「多少人会被挡住」，次数是「挡住的人有多疼」。`.replace` 就是这对数字
分岔得最开的一行——只有 17% 的脚本用它，但用的人用了 142 次。

**排序和上一张表毫无关系，而这正是重点。** 上一张表按语法排，第一名是 `for-of`；
这张表按标准库面排，第一名是 `.contains`，**71% 的脚本、721 次**。
两张表数的是同一个语料。**换一把尺就换一个路线图**——所以「用哪把尺」这件事，
本身就是要被记录和辩护的决定，不能默认。

**第一批（前两行，71% 与 50%）已于 2026-08-29 落地。** 实测代价：
`includes` **320** 字节、`startsWith` **275**、`endsWith` **272**，
对照 `indexOf` 的 **440**。

**`includes` 比 `indexOf` 便宜，而这不是巧合，是设计。** 顺手的写法是
`indexOf(t) !== -1`；那样它会拖进 `units` 这个把字节偏移换算成 UTF-16
码元的辅助件——而**布尔值没有位置要报告**。搜索循环因此是复制的，不是共享的：
共享会让最便宜的方法替最贵的那个付算术。这条有测试盯着
（`includes_is_cheaper_than_index_of_because_it_needs_no_position`），
一旦有人「优化」成调用 `indexOf`，它会响。

**三个方法都在字节层比较，而这是精确的而不是近似的。** UTF-8 自同步且前缀无关
（续字节恒为 `10xxxxxx`，永远不能起头），所以字节序列在某偏移匹配**当且仅当**
码点序列匹配，也不可能从一个字符的中间开始匹配。这是**编码的性质**，
不是对输入的假设——测试拿 `é`（2 字节）和 `😀`（4 字节）钉住了它。
对照 `.length`：同样的捷径在那里是**错的**，因为字符数不等于字节数，所以那个要解码。

**`split`（41%，129 次）同日落地**，代价 **426 字节**（含共享的 `substr` 辅助件与数组集）。

做之前先数了**分隔符本身**——这一步值回票价：129 处里 54 处是 `"\n"`，
其余都是短字面量，而 **`split("")` 零使用**。这直接决定了实现形状。

**空分隔符 trap，而这是表示层逼出来的，不是没做。** ECMA-262 对空分隔符要求切成
UTF-16 **码元**，所以 `"😀".split("")` 是两个孤立代理；本引擎的字符串是 UTF-8，
**没有任何字节序列表示一个孤立代理**。所以合规答案在这里不是「暂未实现」，
是**这个表示法到不了**。两个替代方案都更差：按**码点**切是对最有意思的那批输入
给静默错答案；返回整串是对所有输入给静默错答案。
**零使用让 trap 负担得起，但不是它正确的理由。**

**`toLowerCase`（30%，67 次）同日落地，成交价 8 836 字节**——其中 8 076 是
Unicode 区间表，760 是代码。判决与全部回填在
[`plan/design-case-mapping-decision.md`](../plan/design-case-mapping-decision.md)。

**这一项的价值一半在结论，一半在过程**：

1. **先数改变了题目两次。** 67 次**全是 `to_lower`**，`to_upper` 零使用——
   做成对是花双倍钱买零需求。接收者全是路径与标识符，用途是**不分大小写比较**。
2. **四个选项里三个在某处说谎。** ASCII-only 给 `"CAFÉ"` → `"cafÉ"`（静默错答案）；
   「非 ASCII 一律 trap」把 `café`、中文、emoji 全误报——而**「非 ASCII」不等于
   「有大小写映射」**，要分辨它需要的正是那张表，所以省不掉成本，只是省错了。
3. **估算错了 2.6 倍，而错法比数字值钱。** 事前表里「`i16` delta = 5 384 字节」
   **不可能**（delta 真实范围 −42561..38864，没去量极值）；
   「增量编码 ≈ 3 365」**不可用**（变长编码断了二分查找的定长前提）。
   判据 ④ 事前写的是「记录、不设线」——**幸好是记录而不是上限，否则这里会变成
   一次事后改判据。**
4. **两条具名分歧，第二条是测试抓到的不是设计抓到的**：`İ` 一对多（想到了），
   **词尾 `Σ` → `ς`**（没想到）。判据表是照着「一张表的形状」写的，
   所以它想得到**哪些映射缺失**，想不到**哪些映射是有条件的**。

**价目公开挂着**：用到它的脚本约 **+90%**。若产品判断这个价格不可接受，
正确回应是**不提供这个方法**，而不是提供一个 ASCII 版本冒充它。

### `break` / `continue` 与 `replace` / `replaceAll`（2026-08-29，并行做的一批）

普查第五、六、七行，一次做完——**两者相关性为零**（一个在循环降级，一个在字符串
prefab），所以并行推进、一次验证。

**`continue` 不能跳到 loop 标签。** 那会回到循环**顶部**，跳过 update，
`for (…; i = i + 1)` 会无限转。它需要一个「body 之后、update 之前」的标签，
也就是多包一个块——**只在 body 里真有 `continue` 时才发射**，
一个没有 `continue` 的循环逐字节不变。两个字节的事，但**「零成本」这条规则
一旦有例外就不是规则了**。

**`break` 跨 `finally` 按名拒绝。** 直接 `Br` 出去会让 `finally` **不执行**——
又一次那个「合法但错误」的形状。语料里 `try/catch` 内的 break/continue 有 21 处
（要支持），而 `finally` 全仓只有 2 个脚本 3 处（可以拒）。
**这个比例是先数出来的，不是拍出来的。**

**`replace` 与 `replaceAll` 都做，因为语料写的是前者、意思是后者。**
142 处全长成 `.replace("\r\n", "\n")`——规范化行尾，意思是**全部**替换；
而 JS 的 `replace` 只换第一个。只做一个，无论哪个都会给人一个静默错答案。

**一条被实测打脸的说法**：注释里先写了「两者共用一个函数、只差一个比较，
所以第二个几乎免费」。实测 **`replace` 525 字节，加 `replaceAll` 再 515**。
`Reach` 是编译期选择，**每个名字发射自己的一份**。
**共享 Rust 函数共享的是维护，不是字节**——在 prefab 层这是两种货币。
（合并成带运行期标志的一个函数能给「两个都用」的程序省 515，
却让「只用一个」的程序为一条走不到的分支买单。门控是按方法的，就是为了这个。）

**同时记「零使用」**：`do { } while`、解构、默认参数、**spread**、**`switch`**
在 82 个脚本里**一次都没出现**。这五项是目录驱动会做、需求驱动应当一直往后排的。

### §更正（2026-08-29）：本表尾部两行原先是**假阳性**

`spread ...` 原写 14 个脚本 20 次、`switch` 原写 2 个脚本 6 次。
**剥掉字符串字面量、模板与 `//` 注释后重数，两者都是 0。**
那些 `...` 出现在 `<args...>`、`` `require(...)` ``、`[OBSOLETE_NAME...]` 里，
`switch` 出现在散文里。

**这张表现在**没有一行是「有需求且未做」**了。** 剩下的全是零使用，或已落地。

### 两条「数完之后决定不做」（2026-08-29）

**跨 `finally` 的 `break`/`continue`：语料 1 处，1 个脚本。** 机械是清楚的——
照 `return` 的 `pending` 那套再加两个码即可，约六十行。**但按本仓自己的规矩不该现在建**：
`switch` 零使用就一直排最后，这条 1 处也一样。现有行为已经是**大声点名拒绝**，
不给错答案；真要移植那个脚本的人会撞上一句写明了的诊断，可以改写或来提需求。
**「能做」和「该做」之间隔着一次计数。**

**第四类 fault code：做了，7 字节。** 这条相反——它服务的不是一个特性，
而是**每一次** String 属性误用。原先 trap 出来是裸的 `unreachable`，
和「引擎真坏了」长得一模一样；现在宿主分得清「这引擎没有那个」与「这引擎坏了」。
**字节门当场抓住了这 7 字节**（`arrays_m3.rs` 那条断言从 9 982 变 9 989），
所以数字是带着理由写进测试注释的，不是偷偷改掉的。
只有**走到那条臂**的程序付——写死 key 的那一行仍然不付。

**头部行复查过，一个没变**：`.contains(` 58/721、`.starts_with|ends_with` 41/169、
`.split(` 34/129、`.to_lower(` 25/67、`.replace(` 14/142，`break` 从 56 降到 55
（一处在注释里）。**所以照着这张表建的东西，建在可靠数字上**——被污染的只有还没动工的两行。

**教训是可判定的，不是「以后小心点」**：
**一个模式会不会被散文污染，是这个模式自己的性质，不是语料的性质。**
`.contains(` 带前导 `.` 和尾随 `(`，自带边界；`...` 是标点，`switch` 是裸词，
两者都会出现在人话里。**普查时，裸词与标点的模式必须先剥离字符串与注释；
带定界符的方法调用模式不必。** 这条规则可以事前判断，不必事后发现。
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

## 待办清单（`/goal` 可直接引用这一节）

**这一节存在的理由**：2026-08-29 被问「真的做完了吗」，答案是没有，而当时**没有一处
地方能一眼答出还剩什么**——能力树里既有已做却仍标 `[ ]` 的（`break`/`continue`、
`split`），也有做不了却和能做的混在一起的（真机 iPhone）。清单把两者分开。

**怎么用**：`/goal` 引用本节时，「完成」= 下面每条要么状态变 `[x]`，
要么在本表里带一个**实测数字**写明为何不做。**外部阻塞不算未完成，也不得作为停止的理由。**

### A. 现在就能做，做完可核对

| # | 事 | 为什么（实测） | 做完算什么 |
|---|----|---------------|-----------|
| ~~A1~~ | ~~核里 16 条红测试逐条归因~~ **已归因（2026-08-29）** | 16 条 = **13 条卡带**（缺 `wasm-opt`，Binaryen 未装）+ `fan_c`（clang 的 wasm 链接器缺）+ `webkit`（JSC 差分脚本）+ `ios_wasi_host`（模拟器容器脚本） | **全是本机缺工具链，无一是代码缺陷**。装 Binaryen 与 wasm-ld 后复跑；装不了就在此行写「未跑」 |
| A2 | 金丝雀补成双向——**测试已落地（`f4bf11f`），独立核验只过了一半，还开着** | 它只查「`[x]` 有没有测试」，**不查「`[ ]` 是不是其实做了」**——今天一次抓到 4 条陈旧 `[ ]`。现在 `prd_unchecked_leaves_are_not_already_done` + 手写 `STALE_HINTS`（17 行）在 `mvp_golden.rs` 里。核验方**没有判 holds**：(1) 全套 `cargo test -p tinyvm` 在核验窗口内没跑完；(2) 负控制没复跑；(3) `STALE_HINTS` 的注释说「`7c4f9dc^` 时这些测试都在而 PRD 还写 `[ ]`」，但 `push / pop / map` 那行在 `7c4f9dc^` 已是 `[x]`（本文件第 501 行）——那两行只在下游 PRD 里陈旧过 | 本次更新 PRD 时补测（2026-08-29）：(1) 负控制 `sed '557s/\[x\]/[ ]/'` → 测试红、点名 `break_leaves_the_loop`，前向金丝雀仍绿，PRD 已还原；(2) `tinyvm-qjs` **980/0**，`tinyvm` **297/16**，16 条逐名 = A1 归因的那 16 条。**仍开着**：改掉那句注释（代码改动，不在本次 PRD 更新里做）；改完才打 `[x]` |
| ~~A3~~ | ~~Status 行的版本与测试数上门~~ **已上门（agenterm `bc1a22d5`），且已咬过一次（2026-08-29）** | PRD 36 首行停在 rev `0afc88a`/153，实际 `ec67034`/152，靠人问才发现 | `the_prd_states_the_revision_this_build_pins`：抬 pin 到 `94237cb` 时 PRD 还写 `ec67034`，`cargo test -p agenterm-qjswasm` 红，同一提交里改链才绿——门第一次真的响了 |
| A4 | 一个真实 App target 消费 XCFramework | 验收 #5；打包与冒烟都已绿（`smoke-ios-bridge.sh` exit 0），**只是仓里没有 Xcode 工程** | 仓里有工程且能构建；**不需要设备** |
| A5 | 验收 #4：两个结构不同的游戏跑通确定性回放 | 相关测试正在 A1 的 16 条里 | #4 变绿 |
| ~~A6~~ | ~~`print(非字符串)` 等宿主参数解包在运行期裸 trap~~ **已报名字（2026-08-29）** | `print(s.length)` 编译期不拒（类型是运行期事实），以前在 `unwrap_args` 落进 `unbox_string` 的裸 `unreachable` | 客人写 `"<host>#<n>"` 进 detail 字 + 第六个 fault code；`guest_host_argument` 读回；字面量 String 参数**不再发射标签测试**（比以前更小），其余每个 String 参数位 +~12 B；`I32`/`F64` 参数位同样报名字；`tests/host_argument.rs` 6 条 |
| A7 | 嵌套闭包 + 调用 `import` 进来的函数 → wasm 校验失败 | 下游第一波迁移六组之一测到：`loading wasm: validation: type mismatch`；顶层函数与顶层 try 没事，入口因此都写成平的 | **11 种形状都过**（返回的闭包、`map` 回调、`try` 内、两层嵌套、箭头、字符串参数——`tests/closures_call_imports_m3.rs`，2026-08-29）；报告里没有原始源码，暂不能复现。谁再撞到，把那段脚本贴进来 |
| A8 | `undefined.x` 不可捕获 | ECMA-262 是可捕获的 TypeError；今天是 `unbox_object` 裸 trap，`try/catch` 看不见 | 走 unwind 通道抛可捕获的值；无 `try` 的程序零字节 |
| A9 | V1 装箱下的步数价格 | 下游实测：`"" + x` 每次上千步、`JSON.parse` 每字节 75–107 步、`includes` 每字符 >10 步；16M 默认步数下真仓输入撞顶（现在 CLI 可抬到 100M，但价格本身是上游的） | 先量一张「每个操作多少步」的表进 PRD，再决定优化哪一个 |

### B. 需求为零，**故意不做**（已带数字，不需再决策）

| 事 | 数字 |
|----|------|
| `switch` · spread · `do{}while` · 解构 · 默认参数 | 82 个脚本**零使用**（剥离字符串与注释后重数） |
| 跨 `finally` 的 `break`/`continue` | 语料 **1 处 1 脚本**；现为大声点名拒绝 |
| `toUpperCase` | `to_lower` 67 次，`to_upper` **0 次** |
| `parseInt` | 语义与 `Number` 不同，等按名需求，不做别名 |
| 具名导入 / 默认导出 / 再导出 / 动态 import | 下游 42 处 import **全是**命名空间形式 |
| hex / octal / 分隔符 · tagged template · `for … in` | 语料零使用 |

### C. 外部阻塞——**点名，且不得作为停止的理由**

| 事 | 挡在哪 |
|----|--------|
| 验收 #6：TestFlight 处理与可安装 | Apple 侧处理，需要开发者账号 |
| 真机 iPhone 生命周期 / 帧时 / 音频 / 输入手感 | 需要一台真机 |
| Apple 批准后的外部分发 | 同上 |

### D. 候选，未立项（要先立判决性实验才动）

| 事 | 前置 |
|----|------|
| 原生降级（AOT / JIT） | 四条前置写在「原生降级」那节；先要找到一个真跨过 1500 轮的载荷 |
| typed function references · GC · memory64 · exception handling · threads | 各自要 fixture + 独立 oracle + 体积档，见 P2 |
| slot-B | 未定义 |

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
│   │   ├── `for … of` over an array (13.7.5)             [x] 折成索引循环，无新节点
│   │   │   ├── 元素每轮是新绑定                            [x] 白拿：声明在循环体里
│   │   │   ├── length 每轮重读，body 里 pop 会被看见        [x] 与数组迭代器一致
│   │   │   ├── 字符串 / 非数组按名拒绝，可 catch            [x] 不静默跑零轮
│   │   │   ├── 无声明形式 `for (x of y)`                   [ ] 目标可以是任意赋值目标
│   │   │   └── `of` 当标识符（上下文关键字）                [具名分歧] 词法器仍拒
│   │   ├── `for … in`                                    [ ] 要属性枚举，另一回事
│   │   ├── 模块：`import * as` + `export`（16.2）         [x] 编译期取入，仍是一个 .wasm
│   │   │   ├── 宿主给解析回调，编译器不碰文件系统          [x] 与「核只吃字节」同一条纪律
│   │   │   ├── 命名空间双向不漏（模块↔导入者）             [x] 判据 ③，两条测试
│   │   │   ├── 循环导入点名拒绝，不是栈溢出                [x] 判据 ④
│   │   │   ├── 无 import 的程序零字节                      [x] 判据 ②，与 closures 同一组数
│   │   │   └── 具名导入 / 默认导出 / 再导出 / 动态 import   [ ] 具名拒绝，需求为零
│   │   ├── `break` / `continue`                          [x] 见下方方法段的同名行
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
│   │   │   └── 一个声明执行两次 = 两个绑定 (14.3.1)        [x] 见下
│   │   │       ├── 函数内的 let / const（含 while / 嵌套块） [x] 012，cell 移到声明处
│   │   │       ├── 脚本层的 let / const                     [x] 012，循环内的改走捕获
│   │   │       │   └── 不在循环里的脚本绑定仍是 global       [x] 判据 ④：不许为它涨字节
│   │   │       ├── `for` 头部的 let 每轮复制 (13.7.4.7)      [x] 012，body 之后 update 之前
│   │   │       │   └── `while` 闭包看到末值                 [x] **对照**：333 是正确答案
│   │   │       └── 每多一个循环内被捕获的绑定               [x] +83 字节（斜率，已写成测试）
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
│   │   ├── includes / startsWith / endsWith               [x] 需求普查前两名
│   │   ├── split（非空分隔符）+ 共享的 substr 辅助件       [x] 426 字节
│   │   ├── toLowerCase（Unicode 区间表）                   [x] **8 836 字节**，价目公开
│   │   ├── `slice(start[, end])`：码元位置、负索引、NaN=0、共享核心             [x] 2026-08-29；756 B / 647 B / 两者 1 029 B；代理对内的边界 trap
│   │   │   ├── 不调用它的程序零字节                        [x] 表在门后
│   │   │   ├── 中文 / emoji / 已小写：原样返回不 trap       [x] 判据 ②
│   │   │   └── `İ` 一对多、词尾 `Σ`                        [具名] 两条，见决定文档 §4.1
│   │   ├── toUpperCase                                    [–] 语料零使用
│   │   ├── replace（首个）/ replaceAll（全部）             [x] 525 + 515 字节，各发射一份
│   │   └── `break` / `continue`（无标签）                  [x] continue 自带标签，按需发射
│   │       └── 跨 `finally` 的 break / continue            [–] **先数后决定不做**：语料 1 处
│   │   ├── 第四类 fault code：运行期能力边界                 [x] 7 字节，只有到那条臂的程序付
│   │   ├── `Number(x)`：折成 `+x`，零运行时                 [x] 缺的只是名字，转换早就有
│   │   ├── `Object.keys(o)`：折成 `o.__keys()`，门控 prefab  [x] 208 字节；迁移语料 12 处
│   │   │   ├── 第三种接收者：调用点原只分「数组 / 否则字符串」 [x] 对象接收者曾落进 String 拒绝
│   │   │   └── `for … in`                                    [–] 458 处 for-in 里 352 是数组、172 是区间、12 走 keys()
│   │   │   └── `parseInt`（前缀解析 + 基数）                [ ] 不同函数，按名等需求
│   │   └── spread · switch · do-while · 解构 · 默认参数     [–] 语料**零使用**（假阳性已更正）
│   │   │   ├── 空片段保留（前导 / 尾随 / 连续分隔符）      [x] 最容易漏掉的边界
│   │   │   └── split("") **trap**：孤立代理 UTF-8 表示不了 [具名] **应带第四类 fault code**：下游第一个真脚本撞上时只见裸 unreachable
│   │   │   ├── 字节层比较，对多字节字符精确               [x] é 与 😀 钉住
│   │   │   └── includes 不经由 indexOf，因此更便宜         [x] 320 vs 440 字节
│   │   ├── split [x] · toLowerCase [x] · toUpperCase      [–] 前两个已落地，后者零使用
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
│   │   │   └── the thrown String itself is host-readable (`FAULT_THROWN` pointer, `guest_thrown_message`) [x] 94237cb；下游 agenterm `2cde8b63` 打印它
│   │   ├── a missing String property names itself at run time (`FAULT_MISSING_STRING_METHOD`), not a bare trap [x] 2026-08-29；`slice`/`substr`/`substring` 曾是三个「不同的 bug」
│   │   ├── a host argument of the wrong type names the call and the position (`FAULT_HOST_ARGUMENT`) [x] 2026-08-29；字面量 String 参数不再带标签测试
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

#### P0 的实际状态（2026-08-29 逐步实测，不是「挂起」也不是「已降级」）

这条 P0 被反复问成「挂起还是已降级」，**而那是个假二选一**。逐步跑一遍就散了：

| 步骤 | 需要设备 / Apple 账号？ | 2026-08-29 实测 |
|------|------------------------|----------------|
| 当前 main 为 arm64 iOS 编译 | 否 | ✅ `cargo build -p tinyvm --target aarch64-apple-ios --features ios-c-api` |
| XCFramework 打包（设备 + 两个模拟器切片） | 否 | ✅ `sh crates/tinyvm/build-xcframework.sh <out>` |
| Swift package + Swift/C 冒烟（经 xcodebuild） | 否（模拟器级） | ✅ `sh crates/tinyvm/smoke-ios-bridge.sh` 退出 0 |
| 被一个真实 App target 消费 | 需要一个 Xcode app 工程 | **仓里没有**（可在此机做，只是不存在） |
| TestFlight 处理与可安装 | **是（Apple）** | 外部阻塞 |
| 真机 iPhone 生命周期 / 帧时 / 音频 | **是（设备）** | 外部阻塞 |

链接尺寸（该脚本自己打印，作为回归基线）：
`arm64=1793000 x86_64=1897760 profile-catalog=1625288 replay=1599816
private=1618432 session=1618032 completion=1279504` 字节。

**结论：P0 的前三步是绿的且今天可复跑，第四步只是没人做，只有最后两步是外部阻塞。**
所以正确的记法不是一个状态词，是这张表。

**同时暴露一条真空缺：常规测试口径里没有任何一条跑上面三步。**
2026-08-29 之前连着改了多天 `tinyvm-qjs`，**没有任何检查说 iOS 那侧还编得过**——
今天是绿的，但那是「没碰到核」的运气，不是门在挡。
`smoke-ios-bridge.sh` 一直在仓里，只是没被任何口径引用。
**一个存在但没人跑的门，和没有门的区别，只在于出事时你会更意外。**

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

### 验证口径（2026-08-29 立，之前没有）

在此之前本仓没有写下一条「跑什么算验过」。语言侧一直跑 `cargo test -p tinyvm-qjs`，
而 **iOS 侧那三步一次都没被任何口径引用过**——多天的编译器改动进去时，
没有任何东西说 iOS 还编得过。所以口径写成三条，不是一条：

```sh
cargo test -p tinyvm-qjs --no-fail-fast     # 语言与编译器
cargo test -p tinyvm --no-fail-fast         # 执行核
sh crates/tinyvm/smoke-ios-bridge.sh        # iOS：XCFramework + Swift 包 + 冒烟
```

第三条需要 macOS + Xcode 与三个 rust target
（`aarch64-apple-ios`、`aarch64-apple-ios-sim`、`x86_64-apple-ios`），
**在别的平台上它跑不了，那时要如实写「未跑」而不是跳过不提**。
它打印自己的链接尺寸，那串数字就是回归基线（见 P0 那张表）。

**为什么第三条非有不可**：改编译器时不碰核，看起来与 iOS 无关——
2026-08-29 那次确实无关，绿是运气。**但「这次无关」不是一条能被验证的性质，
而「跑一遍」是。**

**基线（2026-08-29 立）**：第一条 **980 / 0**；第二条 **296 / 16**；第三条 **exit 0**。
`f4bf11f` 加了一条金丝雀之后第二条是 **297 / 16**（2026-08-29 复跑，16 条逐名同上）。

第二条那 16 条**全部既存**，都在游戏卡带、转换器 CLI、webkit 差分与 iOS 模拟器容器上，
需要外部工具链或夹具，本会话没碰过 `crates/tinyvm/src/`。
判据仍是「**失败集合逐条不变**」，不是「失败数为零」。

### 写下口径的第一分钟就照出两条烂账

**一、PRD 金丝雀红着，而它红的正是我自己写的 `[x]`。**
`prd_x_leaves_have_suite_edges` 要求能力树里每一个 `[x]` 叶子都指向一条**会跑的测试**。
它一次列出 **28 片没有测试背书的叶子——全是 2026-08-29 这几天加的**。
声称本身是真的（那些测试都存在且过），但**没有任何东西在核对它们**：
金丝雀住在 `tinyvm` 包里，而大家手打的验证是 `cargo test -p tinyvm-qjs`。
**一个尽职的门，问不到它就等于没有。**

**二、核里的 qjs 哨兵红了很久，没人知道。**
`qjs_m1_rejections_name_the_engine_boundary` 断言 `return 1.5;` 与 `return [1, 2];`
会被拒——而它们分别在 `ab29522`（DecimalLiteral）与 `048bcf2`（数组）就能编了。
**那条测试从那时起就是红的**，同样因为它住在没人跑的包里。

两条是同一个病：**验证口径没写下来，于是它退化成「谁记得跑什么」。**
这一节就是治它的药，而它开出的第一张单子就是上面这两条。

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
- `crates/tinyvm/tests/ios/` — twelve Swift/C smoke sources; `crates/tinyvm/include/`
  — the C headers; `crates/tinyvm/build-xcframework.sh` and
  `crates/tinyvm/smoke-ios-bridge.sh` — the packaging and the gate that runs it.
  (This line said `crates/tinyvm/ios/` until 2026-08-29. That path **has never
  existed** -- `git log --diff-filter=A` finds no commit that added it -- so
  the pointer was to nothing while the artifacts sat one directory over.)

## Explicit non-goals

- H5 mini games, DOM APIs, a WKWebView game shell or JavaScript as the cartridge platform.
- JIT, downloaded native code, cartridge-provided dylibs or device-side AOT on the iOS path.
- WASI as an implicit/default game host surface. An optional, versioned Preview 1 profile may
  be implemented separately; files, network, rendering and storage are never ambient powers.
- WAT parsing in the runtime. Producer tooling may compile WAT to standard `.wasm`.
- Claiming App Review approval from technical validation, TestFlight upload or a release note.
- Replacing `agenterm-dyn`, `agenterm-cu` or `agenterm-chassis`; their product boundaries stay
  separate.
