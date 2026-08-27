# 模板字面量（Template literals）

| | |
|---|---|
| 日期 | 2026-08-27 |
| 目的 | `` `a${b}c` `` 能编译并运行，且模板自由的程序**一个字节都不多付** |
| 实现位置 | `crates/tinyvm-qjs/src/lex.rs`、`src/parse.rs` |
| 前置阅读 | ECMA-262 12.9.6（TV/TRV）、13.2.8（TemplateLiteral）、13.15.3（`+`） |
| 验收 | `crates/tinyvm-qjs/tests/templates_m3.rs`，20 项 |

## §1 为什么没有 `Template` 节点

13.2.8.6 说模板的值 = 各段文本与各替换的 `ToString`，从左到右拼接。
13.15.3（ApplyStringOrNumericBinaryOperator）说：`+` 只要有一边是 String，
做的就是这件事。**两者是同一个算法**。

所以 parser 把模板折成 `+` 链，到此为止——没有新 AST 节点，没有新 emit 分支，
没有新 runtime helper。加一个 `Template` 节点意味着让 emit 用第二种方式回答同一个
问题，而两种答案迟早会分叉（数组里 §2.2 就是这么错的）。

**代价栏因此是空的**，而且是结构性的空，不是靠 gate 挣来的：

| 程序 | 之前 | 之后 | Δ |
|---|---|---|---|
| `return 1;` | 9784 | 9784 | **0** |
| 字符串拼接 | 9836 | 9836 | **0** |
| 对象 | 9873 | 9873 | **0** |
| 数组 | 10632 | 10632 | **0** |
| 闭包 | 10051 | 10051 | **0** |
| JSON | 15448 | 15448 | **0** |

复跑：把 `tests/zz_measure.rs` 写成对上表六个源调用 `compile_qjs_m1().len()`，
在本改动与 `ab29522` 两侧各跑一次。基线取**改动之前的提交**，不是 stash 之后的
工作树——见 `baseline-must-predate-your-change`。

比字节表更强的一条：`templates_m3::a_template_free_program_pays_nothing_for_this_milestone`
断言模板与它等价的拼接编译出**逐字节相同的模块**。

## §2 折叠规则，以及为什么头部那个 `""` 必须留

折叠从**头部文本的字面量**起手，即使它是空的；之后每一段空文本都丢掉。

- 留头部：`` `${1}${2}` `` 必须是 `"12"` 而不是 `3`。只有让最左操作数是 String，
  第一个 `+` 才是拼接。丢了它就是 `1 + 2`。
- 丢其余：一旦最左是 String，整条链的运行值**永远**是 String，`+ ""` 就是恒等。
  于是 `` `a${b}` `` 正好折成 `"a" + b`——手写会写的那个式子。

两条都在 `an_empty_piece_after_the_head_is_dropped_and_the_head_is_not` 里钉死。

## §3 词法器：`}` 的两个意思

`tokenize()` 是**一趟平坦循环**，一次跑完整个输入交给 parser 一个 `Vec<Token>`。
所以模板不能是"一个装着未词法化源码的 token"——那需要第二个词法器入口，两个会漂移。

模板改为**四个 token 的族**：`TemplateFull` / `TemplateHead` / `TemplateMiddle` /
`TemplateTail`，替换里的表达式就是**普通 token**，夹在中间。

代价是 `}` 有了两个意思：收掉一个块，或者接回模板文本。分辨它们靠
`Lexer::templates`——每个 `${` 开着的模板一帧，记录反引号位置（给诊断用）和当前
替换里已经开了几个 `{`。深度为 0 的 `}` 才是"接回模板"。

这就是 `` `${ { a: 7 }.a }` `` 与 `` `a${`b${c}`}d` `` 能过的原因，也是它必须是
**栈**而不是一个 flag 的原因。

## §4 本里程碑刻意没有的东西

| 缺的 | 为什么 |
|---|---|
| 带标签模板 `` t`a${b}` `` | 它是**调用**，第一个实参是一个冻结的 cooked 字符串数组、带 `raw` 属性。本引擎既没有数组方法也没有属性定义，做不出那个形状。已在 `postfix()` 里**按名字拒绝**——不这么做的话 `` t`a` `` 会被报成"缺分号"，作者会去找错地方。 |
| `String.raw` | 同上，且它需要一个全局 `String`。 |
| `"ab".length` 之类 | 与模板无关：本引擎没有 String 原型，属性访问在**运行时**陷入（`objects_m3::a_property_of_a_non_object_traps`）。模板原样继承这一条——这正是"模板就是 String"的另一种说法。 |

## §5 走掉的拒绝行

模板落地后，三处拒绝语料里的 `"template literals"` 行按各自预写的替换规则搬了家：

- `conformance_m2::UNSUPPORTED` —— 删行，行为进 `templates_m3.rs`。
- `closures_m3::the_neighbouring_constructs_are_still_refused_by_name` —— 删行。
- `objects_conformance` —— 那一行是 `` `${o.a}` ``，改成**行为断言**：现在返回 `"1"`。
- `adversarial_m2::guard_unterminated_lexemes_are_named_not_crashed` —— 未闭合的模板
  不再是能力边界，而是**格式错误**，指向那个没被闭合的反引号（`byte 7`）。

`TokenKind::capability()` 仍为这四个 token 保留 `(Subset, "template literals")`：
词法器毕业了，但 M0 前端仍需要一个名字来称呼它拿不动的东西。
