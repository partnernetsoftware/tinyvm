# 箭头函数（Arrow functions）

| | |
|---|---|
| 日期 | 2026-08-27 |
| 目的 | `(x) => x + 1` 与 `x => x` 能编译并运行；**无箭头的程序不多付字节，也不多付编译时间** |
| 实现位置 | `crates/tinyvm-qjs/src/lex.rs`（`=>` 成为真 token）、`src/parse.rs` |
| 前置阅读 | ECMA-262 15.3（ArrowFunction）、13.2.2（CoverParenthesized…） |
| 验收 | `crates/tinyvm-qjs/tests/arrows_m3.rs`，13 项 |

## §1 为什么没有 `Arrow` 节点：一个**有条件**的等价

在**这个**引擎里，箭头函数就是函数表达式。15.3 用来分开两者的每一条，
都伸手去够这个引擎没有的东西：

| 15.3 说箭头没有… | …而这个引擎没有 |
|---|---|
| `this` 绑定 | `this`——按名字拒绝 |
| `arguments` 对象 | `arguments`——一个未声明的名字 |
| `[[Construct]]` | `new`——按名字拒绝 |
| `prototype` 属性 | 函数属性本身（`f.length` / `f.prototype` 运行期 trap） |

所以 parser 建的就是 `ExprKind::Function`，没有新节点、没有新降级、没有新 runtime。

**但这个等价是有条件的**：条件是上面那四个「没有」。哪天 `this` 落地，等价就作废，
而且是**悄悄**作废——箭头会继承外层 `this`，普通函数不会。
`arrows_m3::the_absences_the_arrow_equivalence_rests_on` 把四条都钉住，
所以那天会**响**。这一条是本设计里最重要的一句话，不是脚注。

字节：**箭头与它意思相同的函数表达式编译出逐字节相同的模块**（四组，含捕获的那组）。
无箭头程序 Δ 全 0（八种程序，基线取 `653cebe`）。

## §2 覆盖文法：`(a, b)` 还是 `(a, b) =>`

`(` 之后到 `)` 之前，没有任何东西能说明这是分组还是参数表。ECMA-262 的办法是
13.2.2 的覆盖文法：先按一个既非此又非彼的产生式解析，等 `=>` 出现再**重新解释**。

本 parser 手上有**整个 token 向量**，所以可以在解析**之前**就把问题定死：
从这个 `(` 走到与它配对的 `)`，看后面是不是 `=>`。两种读法因此从不同时存在，
也就没有「在一种读法下建好的节点被另一种读法复用」——覆盖文法最容易微妙出错的正是那里。

箭头只在 `expression(min_bp)` 里 `min_bp <= BP_ASSIGN` 时被识别，因为 15.3 把
ArrowFunction 放在 AssignmentExpression 那一级。这条不是洁癖：它正是
`1 + x => x` 仍然是语法错误的原因——`+` 的右操作数以更高的 binding power 解析，
那里根本不问这个问题，于是 `=>` 落单。接受它就是这个引擎自己发明文法。

## §3 这个里程碑真正的代价是**编译时间**，不是字节

配对扫描原本会在**每一个** `(` 上跑。实测（release，20 次编译，取热身后的稳定值）：

| 源 | 无本改动 | 朴素实现 | 加 `has_arrow` 后 |
|---|---|---|---|
| `scripts/qjs/lib/fleet.qjs`（真实文件，无箭头） | 22.2 ms/200 | ×2.2 | 22.8 ms/200（噪声内） |
| 200 层嵌套括号 | 2.13 ms/200 | ×6.9 | 2.39 ms/200（+12%） |
| 400 条 `s = s + (i);` | 117 ms/200 | ×1.7 | 120 ms/200（+3%） |

`Parser::has_arrow` 在构造时对 token 向量扫一遍，记下源里到底有没有 `=>`。
没有的话，`at_arrow_head` 立刻返回 false，配对扫描一次都不跑。

这跟 runtime 预制件的门是同一条规矩——**不用这个特性的程序不为它付钱**——
只是这次付的是编译时间而不是字节。剩下的 +12%（只在病态嵌套括号上）是
`expression()` 里多出的那个分支，如实记在这里。

复跑：`tests/zz_measure.rs` 写成对上表三个源各编译 200 次并计时，
在本改动与 `653cebe` 两侧各跑三轮，丢掉第一轮。

## §4 本里程碑刻意没有的东西

| 缺的 | 为什么 |
|---|---|
| 默认参数 `(a = 1) => a` | 参数表的语法，跟函数的参数表是同一件事，一起排期 |
| rest / 解构参数 | 同上；`...` 已有具名拒绝 |
| `async` 箭头 | `async` 关键字整体未支持 |
| `this` | 见 §1——它落地那天，本文件 §1 必须重写 |

## §5 走掉的拒绝行

- `conformance_m2::UNSUPPORTED` —— 删行。
- `closures_m3` / `templates_m3` 的 neighbouring 表 —— 删行。
- `function_conformance::an_arrow_function_is_refused_by_name` —— 改写成
  `an_arrow_function_is_a_function_expression`，并断言两种写法编译成同一个模块。
- `function_values::an_arrow_function_is_refused` —— 改写成
  `an_arrow_function_is_a_function_value`。
- `function_conformance::every_refusal_in_this_area_speaks_for_the_engine` —— 删三行。

两处旧测试里记着一个**疣**：`() => 1` 当时会让 parser 在读到 `=>` 之前就没了操作数，
于是那句诊断不提箭头。它现在能解析，疣跟着没了。

`TokenKind::capability()` 仍为 `FatArrow` 保留 `(FullJs, "arrow functions")`：
M1 前端能建了，M0 前端还需要一个名字来称呼它拿不动的东西（`lex_m1.rs` 那张表因此原样绿）。
