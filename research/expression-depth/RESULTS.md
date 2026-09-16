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

## decode budget 现在是 `Limits` 字段（2026-09-16）

下游实测：`entry + 全部 reserved modules` 在 expression_depth **开启**时是
**781,705 bytes / 347,290 decode items**，超过加载器固定的 **262,144**（+32.5%）⇒
`module decode budget`。off 侧未直接测，按插桩放大率反推约 **93,100**（≈35%）。⇒ 这是
**插桩放大 × 固定预算**的容量冲突，与上面的 `throw_check` 修复无关（该修复只删指令）。

修法：把该上限从常量变成公开 `Limits` 的字段 **`max_decode_items`**（默认仍
`WASM_MAX_DECODE_ITEMS = 262_144`），`DecodeBudget` 从**传入的 `Limits`** 取值。
⇒ **默认行为逐字不变**；需要解码更大模块的嵌入者（例如加载自带 entry + reserved
modules 的产品）显式提高即可，而不必放宽全局默认。host profile 的 wire 格式**没有**
该字段，profile 装载的模块继续用默认上限。

证据（`tests/decode_budget_limit.rs`，手编码一份字节供三向共用）：同一份
300,000 个 no-op 的函数体 —— 默认 `Limits` **拒绝**（`module decode budget`）、
显式 `524_288` **加载**；`max_decode_items: 0` 与 `4` 均 **fail-closed**。
既有 `tests/untrusted.rs` 的 count-bomb 语义（默认上限下的拒绝）保持不变。

## H3 归因：插桩增长是固有成本 —— 判负（2026-09-16）

下游 PRD36 H3 的开放问题是：expression-depth 插桩把解码项推过默认上限（生产 closure
347,290 > 262,144），这是**重复计费**还是**真实发射的程序**。本节是一次固定 guest 的
单次归因，判决为**后者**，因此**不立优化叶**。判据与时间盒记在下游 sibling 仓
`agenterm` 的 `plan/design-qjs-expression-depth-decode-cost-experiment.md` 里。

**基线** `9420045`（本节测量时 tinyvm 的 HEAD），与上文各节同工具链。

### 固定 guest 与唯一变量

- guest：本目录 G4 用的右嵌套生成器，`levels = 64`，源 = `return 1 + (1 + (…));`。
- 唯一变量：`RuntimeLimits { expression_depth: off | on }`；同一编译器、同一
  `Options::default()`、同一 rev。
- 测量单位：**decoder 自己的口径**。两个公开装载入口都经 `Module::decode`，而它只建
  **一个** `DecodeBudget`（`wasm.rs:5045`）；section 流与每个 code body 都从这同一个实例
  扣（`wasm.rs:5111` 的 `parse_code_section` 与 `wasm.rs:5157` 的 `decode(&expr, …)` 传的是
  同一引用），所以「能加载该 artifact 的最小 `max_decode_items`」就等于它的总 charge。
  二分求得，不引入第二个计数实现。（`Module::add_function`（`wasm.rs:4918`）是宿主逐
  body 自建模块的另一条路径，它给每个 body 独立的 budget —— 与本节的整模块口径无关。）

### 数字

| 变体 | bytes | charged items | items/byte |
|---|---|---|---|
| off | 11,095 | 5,666 | 0.510680 |
| on | 15,028 | **7,870** | 0.523689 |
| Δ | +3,933 | **+2,204** | +0.013009 |

on 侧 15,028 bytes **逐字复现**本文件 G4 上一节记录的字节数 ⇒ 同一 guest、同一 options
的交叉校验；off 侧 11,095 是本节的数字。生产 closure 的 347,290 / 781,705 = 0.444274
items/byte 与这两行同阶（差 15–18%）—— 这只是**记录性对照**，**不作判据**（见下"限制"）。

### 公式与精确拆分

```text
ΔT = shared + checks · σ
2204 = 11      + 129     · 17
```

- **计费口径（源码事实）**：`decode(body)` 的循环体第一句就是 `budget.charge(1)`
  （`wasm.rs:2290`）⇒ **每条 opcode 恰好计 1 项**。该函数体内**唯一的第二处**计费是 `0x0E
  br_table` 的目标表：按条目数 `budget.charge(count)`（`wasm.rs:2788`）—— 这正是
  `Limits::max_decode_items` 文档里 "per immediate group" 所指的那个 immediate group；
  普通立即数（`i32.const` 的 LEB 等）不另计。而 tinyvm-qjs 的**生产发射路径不发射
  `br_table`**：`encode::br_table`（`encode.rs:953`）只被测试调用，且 `emit.rs:293-295`
  把"把每个函数体变成 `loop` + `br_table` 的 handler 表"明确记为一个被否决的方案。
  ⇒ 对本节 guest，一个 body 的计费项数等于它的 opcode 数。
- `checks = 129`：G4 记录的 dynamic checks（`2N+1`，N=64），即被求值的 AST expression
  数；enter/leave 是逐 expression 的，故为 129 对。
- `σ = 17`：一对 enter/leave 的计费项，由 `emit.rs` 的形状直接给出 ——
  `enter_expression`（`emit.rs:4316`）13 条 opcode：`global.get active`、`global.get limit`、
  `i32.ge_u`、`if`、`store_fault` 的三条（`record_expression_depth_exhausted`）、
  `unreachable`、`end`、`global.get active`、`i32.const 1`、`i32.add`、`global.set active`；
  `leave_expression`（`emit.rs:4333`）4 条：`global.get active`、`i32.const 1`、`i32.sub`、
  `global.set active`。
- `shared = 11`：`ΔT − 129·17` 的差，即**不随被求值表达式数增长**的剩余部分。本节**没有把
  它逐项归名**：只能说它的量级与上文 G4 已具名的三类共享构造（一项 function import 声明、
  两个 private globals、invocation prologue）**一致** —— 这是一次**相符性观察**，
  **不构成**"由它们承担、因此无残差"的证明。

（先行版本把 σ 夹在 `[16, 17.1]`，并推测过"立即数组可能另计"。读码后区间收成精确值，
且 immediate group 的唯一实例是 `br_table` 的目标表，而本 emitter 不发射它。）

### 判决 trace

1. **无重复计费 —— 只用源码硬事实**：`decode(body)` 对每条 opcode 只执行一次 `charge(1)`
   （`wasm.rs:2290`，循环首句），该函数体内唯一的另一处计费是 `br_table` 的目标表
   （`wasm.rs:2788`），而本 emitter 不发射 `br_table`（见上）。⇒ 代码里**不存在**让同一条
   opcode 被计两次的路径。
2. **独立交叉校验**：decoder 实测 `ΔT = 2,204`；按形状算出的插桩账是
   `129 · (13 + 4) = 2,193`。两者相差 **11 项**，即不随被求值表达式数增长的那部分。
3. **残差未被逐项归名**：这 11 项的量级与 G4 已具名的三类共享构造一致（相符性观察）；
   本节**不能**断言它们"由那三类承担、因此无残差"。
4. **无可折叠构造**：enter 的 limit 比较与 `active` 递增、leave 的对称递减都是该语义必需，
   未发现"发射了但可省"的重复构造。

⇒ 插桩的计费恰好等于它自己发射的 opcode 数（每对 13 + 4），外加一个不随表达式数增长的
小共享量。观测到的增长**就是真实发射的程序**，是这项插桩的**固有成本**。**判负**：不立
优化叶，不以上调上限来掩盖它（与 PRD36 的 "a larger limit is not an optimization" 一致）。

### 限制（本节未做，不得据此推断）

- **本节证明的是计费规则，不是对 emitted 字节的逐项对账**：仓内无 wasm 解析器
  （tinyvm-qjs 的 dev-dependencies 只有 `wat` 与 `serde_json`；`FeatureUsage` 是提案级
  布尔，`Function` 不暴露指令），所以"整模块 charge = 该模块的 opcode 总数"这一条**未逐项
  核对**；被证明的是"规则里没有第二条给同一条 opcode 计费的路径"。
- **`items/byte` 只是记录，不是判据**：bytes 里含不计费的立即数、长度前缀与 section 头，
  所以该比值既不能推出"存在重复计费"，也**不能**推出"不存在重复计费"——先行版本曾用它
  充当后者的证据，该论据已撤销。
- `checks = 129` 依赖 G4 那里的"dynamic checks = 被求值 expression 数"口径；若该口径变化，
  应重算 `σ` 的倍数关系，而不是沿用 129。
- `shared = 11` 未逐项归名（见 trace 第 3 条）。
- 本节只判"是否存在重复计费/可折叠构造"，不覆盖吞吐、冷启动、常驻尺寸或六格运行期证据。

### 复跑

探针是临时文件，测完即删；它的全部内容如下，写到
`crates/tinyvm-qjs/tests/zz_decode_attribution_probe.rs` 即可复现：

```rust
use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Options, RuntimeLimits, compile_qjs_m1_with_runtime_limits};

fn nested(levels: usize) -> String {
    let mut expression = "1".to_owned();
    for _ in 0..levels {
        expression = format!("1 + ({expression})");
    }
    format!("return {expression};")
}

fn charged_items(bytes: &[u8]) -> usize {
    let loads = |ceiling: usize| {
        WasmModule::from_bytes_with(
            bytes,
            Limits { max_decode_items: ceiling, ..Limits::default() },
        )
        .is_ok()
    };
    let mut low = 0usize;
    let mut high = 1usize;
    while !loads(high) {
        high = high.checked_mul(2).expect("a doubling ceiling must load");
    }
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        if loads(mid) { high = mid } else { low = mid }
    }
    high
}

#[test]
fn probe_140_decode_charge_off_and_on() {
    let source = nested(64);
    for (label, expression_depth) in [("off", false), ("on", true)] {
        let bytes = compile_qjs_m1_with_runtime_limits(
            &source,
            Options::default(),
            RuntimeLimits { expression_depth, ..RuntimeLimits::default() },
        )
        .expect("the fixed guest compiles under both option sets");
        let items = charged_items(&bytes);
        println!(
            "PROBE-140 {label}: bytes={} charged_items={} items_per_byte={:.6}",
            bytes.len(),
            items,
            items as f64 / bytes.len() as f64
        );
    }
}
```

```sh
cargo test -p tinyvm-qjs --test zz_decode_attribution_probe -- --nocapture
```

期望输出即上表的四行原文（bytes 与 charged items 是记录，不是未来的固定承诺；若
expression taxonomy 或计费规则改变，应在同一工具链、同一 options 下刷新）。
