# Expression-depth runtime limit — A 已判决

| 项 | 值 |
|---|---|
| 日期 | 2026-09-16 |
| 基线 | `6cff7d49678fa708a9e46e4db2053232656e7b0e` |
| 工具链 | `rustc 1.97.0 (2d8144b78 2026-07-07)` · `cargo 1.97.0 (c980f4866 2026-06-30)` |
| 判决 | **A：guest mutable globals + invocation-scoped runtime-limit import** |

本实验回答一个问题：能否精确限制“当前 JavaScript 函数内同时活动、尚未完成的
expression evaluation chain”，同时不向函数调用帧、activation slots 或未执行分支收费。
结果是能；B（隐藏函数参数）没有触发，C（复用现有调用/slot 计数）保持判负。

## 判决 trace

1. 静态预审找到了 invocation entry、direct/indirect call、table adapter、throw/catch/finally
   的恢复落点，没有发现必须改变 guest 函数 ABI 的边。
2. A 的最小实现使用两个 gated mutable globals：一次 invocation 开头从
   `tinyvm_qjs_runtime.expression_depth: () -> i32` 读取 ceiling；另一个 global 保存当前
   函数的活动表达式深度。每个表达式的 enter/leave 都是 guest 指令，不发生逐表达式
   host 往返。
3. direct call、indirect call 与 runtime prefab callback 都在调用边保存 caller 深度、为
   callee 建立零基线并在返回后恢复。最后一条是实现中推翻的初始遗漏：只处理普通
   lowering 会漏掉 `map` 等经 table adapter 回调 JavaScript 的路径。
4. throw 路径在跳到 handler/finalizer 或离开函数前清零；catch 入口再次建立明确零基线。
   finalizer 与 return/finally 的动态反例均保持原结果。
5. G1、G2、G3、G5 全过；按事前判决树选择 A。G4 只报告成本，不推翻语义结论。

## G1–G5

| 门 | 结果 | 证据 |
|---|---|---|
| G1 语义矩阵 | **PASS** | 4 层 exact-limit 成功、3 拒绝；32 层递归浅表达式成功；dead logical/conditional 分支不收费；direct、indirect、`map` callback 均有 exact 成功与 limit-minus-one distinct fault；当前函数 catch、跨函数 throw、normal/throw finally、return+finally 精确结果全对 |
| G2 reusable bytes | **PASS** | 同一 `Vec<u8>` 在 ceiling 4 成功、3 返回 `ExpressionDepthExhausted`；ceiling 不进入 artifact |
| G3 ABI 漂移 | **PASS** | A 不改变已有 user function 参数/结果、table element signature 或 export；只在 opt-in artifact 追加一个 function import与两个 private globals；普通 API bytes 与显式 `RuntimeLimits::default()` bytes 全等 |
| G4 emitted growth | **报告** | 8 层：10,884 B、17 个动态 expression checks；64 层：15,028 B、129 个 checks。共享成本是一项 import、两个 globals 与 entry prologue；逐表达式成本是一对 enter/leave guest instruction runs |
| G5 abrupt edges | **PASS** | 已列并执行 direct call、indirect call、table callback、runtime-raised catchable throw、explicit throw、catch entry、finally normal/throw、return+finally、invocation entry；无未解释旁路 |

这里的动态检查数是测试源码真正求值的 AST expression 数：`N` 层右嵌 binary 含
`N` 个 binary 与 `N+1` 个 literals，所以分别为 `2N+1`。它不是 Rust 测试二进制大小，
字节数来自同一编译器与同一 options 生成的 wasm artifact。

## Safe failure 与适用边界

- 超限先写独立 fault 12，再执行 `unreachable`；host 读到
  `GuestFault::ExpressionDepthExhausted`，不会误报 heap、call depth 或 activation slots。
- limit import 未绑定时，wasm import resolution 响亮失败；opt-in 关闭时不声明 import、
  不增加 globals、不插指令，而且 byte-equality test 守住该事实。
- fault 不可被 JavaScript `catch` 捕获；它是 embedding runtime ceiling，不是脚本 throw。
- 这项判决只覆盖 tinyvm-qjs 通用机制。数值来源、产品 audit、用户错误投影与平台证据
  仍由下游 embedding 自己拥有。

## 复跑

从 tinyvm 仓库根目录：

```sh
cargo test -p tinyvm-qjs --test expression_depth -- --nocapture
cargo test -p tinyvm-qjs
cargo clippy -p tinyvm-qjs --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

定向测试的 G4 输出应报告本次发现集的两个 artifact 字节数与 17/129 checks；测试数量和
artifact bytes 是测量记录，不是未来固定承诺。若 expression taxonomy 或编码改变，应在同一
工具链/同一 options 下刷新，而不是沿用旧数字。

## 非目标

- 不在 tinyvm 中加入 AgenTerm 名词、预算默认值或 audit；
- 不把 limit 烘进 artifact，不新增 guest export，不占固定 memory word；
- 不用静态 AST 最大深度、call depth、activation slots 或 fuel 近似本事实；
- 本次不做插桩性能优化。若后续真实 workload 证明 G4 成本需要优化，应另立测量叶，且不得
  放宽 G1/G2/G5。

## 后续修复：modules × expression-depth 的 load 期失败（2026-09-16）

把该 limit 接到下游（`agenterm-qjswasm` 打开 `RuntimeLimits { expression_depth: true }`）后，
**modules 与 throw 的组合**在模块加载期失败。四面夹逼（`tests/expression_depth.rs`，四条各
自 PASS/FAIL）：

| 形状 | expression_depth | 结果 |
|---|---|---|
| module 内调用一个会 `throw` 的函数 | on | **非法**：`validation: operand stack underflow` |
| 同一段源码内联、无 module 边界 | on | 合法 |
| module + throw，两个 runtime limit 全关 | off | 合法 |
| module 内 `throw` 但**没有**内部调用 | on | 合法 |

**根因**：`throw_check`（call 之后的 throw 检查）在 limit 开启时把两条指令写成了

```wat
global.get $unwind_flag
if (empty)                 ;; ← 这个块是根因
  <reset_expression_depth>
  br $target
end
```

`Lower::push` 对 `Ins::If(_)` 会 `self.depth += 1`，而 `unwind_target()` 用 `self.depth` 计算
**相对标签**：有 handler 时 `self.depth - at`，**没有 handler 时就是 `self.depth`**。⇒ 那个块
让无 handler 的 throw 标签**多跳一层**，生成的模块因此非法。有 handler 时 `depth - at` 恰好
抵消多出的那一层，所以只有"无 handler 的 throw"路径触发——这也解释了为什么四格中只有第一格红。

**修复**：`throw_check` 无论 limit 是否发布都**只发两条指令**（`global.get $flag` +
`br_if $unwind_target`），**不引入任何块**。这里本来也不需要恢复：callee 抛出时已经在
`throw_stmt` 里重置了自己的链，正常返回则按设计保留 caller 的链。

**判据**：`cargo test -p tinyvm-qjs --test expression_depth` = 9 passed / 0 failed（上述四条 +
原有五条），`--test modules_m3` = 16 passed / 0 failed。
