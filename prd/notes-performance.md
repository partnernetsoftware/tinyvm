# 底层性能（笔记 · 2026-08-22）

政委：到时死磕 tinyvm 底层性能。除了汇编层手搓，先记下方案。不是当前写刀。

不 JIT（含 iOS）。先别上手搓汇编。最快的是装载期把 wasm 降成更密的内部码，解释器再死磕调度。汇编留给那一圈热循环。

## 三条

1. **装载期 lower**  
   校验时融 opcode、能消的 bound check 消掉，跑的时候别再解析。

2. **解释器本身**  
   computed goto、栈顶缓存、超指令。对照 wasm3 和 WAMR fast interp，只吃设计，不搬仓。

3. **以后才开的分叉**  
   桌面可 AOT 成本机码，iOS 仍解释。那是槽 B，现在停着。

## 已落地（2026-08-23）

先有门再有刀：`smoke-interpreter-throughput.sh` 量「每条 guest 指令多少纳秒」，
八种指令组合，计时前先让 wabt 独立算一遍答案。
证据在 [docs/tinyvm-interpreter-throughput.md](../docs/tinyvm-interpreter-throughput.md)。

第一刀是采样指出来的，不是猜的：`local.get` 里的 `push_operand` 占了五分之一时间，
因为每次压栈都调一次 `Vec::try_reserve`；算术助手则用三次 Vec 边界检查去做一次
「栈少一格」。直线代码快了 12–30%，调用密集的两行几乎没动——它们的成本在
activation 建立，不在操作数搬运，那是下一刀。

体积门在这里决定了刀的形状：把栈处理摊进泛型助手会让静态核从 101,240 涨到
117,752 字节，超 100 KiB 上限 15 KiB（助手对闭包泛型，~150 个调用点各一份）。
改成走非泛型共享函数后同样快，核只 +16 字节。可移植性排在吞吐前面，
门没有被重新谈判。

## 体积余量是下一刀的前置

f64 的原地改写量过了：−27.7%，和 i64 一个量级。但它要 ~200 字节 __text，
而核只剩 1.1 KiB 余量，且链接产物随 __TEXT 过页按 16 KiB 跳变——192 字节
文本增长量出来是 16,512 字节文件增长，直接超限 15 KiB。三种写法都一样。
TinyArcade 是 i32-only，f64 不在关键路径，<100 KiB 才是「无 JIT 也能活」的
本钱，所以撤回。

结论：**先买回体积余量，再谈吞吐**。助手对闭包泛型、~150 个调用点各一份，
是余量被吃掉的大头；改成函数指针能省很多但会加一次间接调用，需要拿门量过
再决定。谁买回余量，第一件事就该把 f64 那刀捡回来——它是已测的 27.7%。

## host binding 单态化：已判负（2026-09-14）

上面那条未决项在下游有了一个可判决的实例，规格与结论见
[`plan/design-host-binding-monomorphization-experiment.md`](../plan/design-host-binding-monomorphization-experiment.md)。
状态：**已判负——截距 0 KiB 触发提前出口，斜率与延迟未到达**。

判决：把擦除后的扫描/安装循环抽成私有非泛型 helper，**没有**产出可测收益。
A/B 两轮实测均为净差 **0 KiB**（`bind_import_typed` 仍 211 实例 / 69.7 KiB，`tinyvm`
crate 仍 140.5 KiB，`.text` 总 5133.1 KiB 不变；二进制 10,108,664 B → 10,108,648 B，
−16 B 属对齐/噪声量级）。第二轮的 helper 加了 `#[inline(never)]` 后仍然如此。

**最有价值的发现是否定且可复用的：绑定期私有 helper 在这次 thin-LTO 构建下没有
带来可归属的体积下降，即使加 `#[inline(never)]` 也一样**（`nm`、`strings` 与
mangled 片段搜索均为 0 处，tinyvm 自身 rlib 及其解包 object 中也搜不到）。构建可能
内联、internalise 或合并了它；本实验不区分这些优化动作。后来者不应在没有新证据时
重试同一形状。

这也排除了扫描/安装循环作为 211 实例 / 69.7 KiB 中可独立抽取的可测成本，但不能据此
唯一归因剩余字节。若未来要审公开泛型边界（例如新增一个接收擦除后 `Rc` 或函数指针的
入口），那是另一项 API 与延迟实验，超出本实验的事前约束。

本判决只排除「抽取扫描/安装循环」这一形状；**不排除**未来在公开泛型边界有新证据时
另立实验，也不涉及 `bind_import_typed_in_place` 与 bounded 家族（本实验未触碰），
亦不涉及 `staticcore` 构建。

判据口径（四元）：{边界 下游 `agenterm` bin 的 `.text`，含 `tinyvm` 与 `tinyvm_qjs` 两个 crate 的全部
符号归属；工具 `cargo bloat`（DWARF crate 归属）；构建 `opt-level="z"` + `lto="thin"` +
`codegen-units=1` + `panic="abort"` + `strip="none"`（strip 只影响符号表）；目标 macOS arm64
release，**该构建仅做字节测量、未被执行**}。复跑命令见规格 §8.7。

下面保留实验期间的归因证据（它们仍是有效证据，但**不是**本实验的结论）：

下游 `agenterm-qjswasm` 的绑定调用点与实例：

| 口径 | 数 |
| --- | ---: |
| `HostFn` 声明 | 11 |
| 闭包来源（53 个 `tool.rs` 的 `bind_metered` 调用 + 11 个 `host.rs` 直接 `bind` + 2 个 `tool.rs` 直接 `bind`） | 66 |
| 去重后的不同闭包类型 | 66 |
| `bind_import_typed` 符号/内联实例 | 211（69.7 KiB，占该 `.text` 1.36%） |

实例体积高度偏斜，均值 338 B、中位 120 B、最大 5325 B：

| 分档 | 实例 | 字节 | 占比 |
| --- | ---: | ---: | ---: |
| <128 B | 151 | 10,916 | 15.3% |
| 128–256 B | 2 | 340 | 0.5% |
| 256–512 B | 6 | 2,596 | 3.6% |
| 512 B–1 KiB | 29 | 22,004 | 30.8% |
| ≥1 KiB | 23 | 35,533 | 49.8% |

可擦除区间与判据的初步读数：必须保留的是 <128 B 的 151 个薄适配（10.9 KiB）；可能共享的是
≥512 B 的 52 个（57.5 KiB）。理论上界 61.7 KiB，只需共享其中 52% 即达规格里 32 KiB 的截距门。
现状每 10 个同签名 op 的总斜率约 3.30 KiB。

本轮还修掉了两处归因错误，记录在此以免被重复：初始用 `otool -v -t` 相邻符号地址差归因，
得 `tinyvm` 156.8 KiB / `tinyvm_qjs` 247.0 KiB，比 `cargo bloat` 系统性偏高约 13%——原因是把
下游内联进本 crate 的泛型实例错算给 `tinyvm`；macOS 上 `nm -S` 的 size 字段恒为 0（Mach-O
`nlist_64` 不带 size），两者都不可用作主口径。`cargo bloat` 给出 `tinyvm` 140.5 KiB、
`tinyvm_qjs` 216.1 KiB，与本文件上面那条先例所处量级一致（两者口径不同，只作量级印证，
不得跨口径相除）。另有一次中间回报声称 `nm` 检出 1 个 `install_typed_returning` 符号；
同命令重跑、`strings` 与严格 mangled 片段搜索均为 0，那次是误读回显，正确值为 0。

## 核里的 68 KiB 花在哪（2026-08-23 实测）

staticcore + `-Copt-level=z` 链接后按符号排：

| 字节 | 符号 |
| ---: | --- |
| 17,524 | `Module::from_bytes_with`（解码器，冷路径，最大的一块） |
| 15,192 | `Module::run_defined`（解释器） |
| 4,204 | `wasm::decode` |
| 3,016 | `tinyvm_selftest`（体积门自己的入口） |
| 2,556 | `Module::call_any_until_boundary` |
| 1,292 | `BTreeMap<String, usize>::insert` |
| 1,220 | `parse_const_expr` |
| 640 + 568 + 436 | `fmt::Formatter::pad_integral`、`str::from_utf8`、`do_count_chars` |

两条可动的线索：

1. **自称 fmt-free 的 crate 里有 fmt。** 来源是会 panic 的下标/切片操作——
   越界 panic 的消息要格式化两个整数，于是整套 fmt 机器被链进来。
   这不是本次改动引入的，基线同样有。清掉要把 wasm.rs 里约 86 处标量下标 +
   20 处切片区间（其它文件另算）全换成 `.get()`，**全有全无**：剩一处 fmt 就还在。
   顺带一提，这跟 PRD 的「坏模块必须返回类型化错误、绝不 abort 进程」是同一件事——
   iOS C ABI 那条路有 catch_unwind 兜着，`staticcore` + `panic=abort` 的嵌入没有。

2. **解码器比解释器还大。** 冷路径占了四分之一的核。要余量先看它。

买回余量后第一件事是把 f64 那刀捡回来（已测 −27.7%）。

## 门

globals/locals 那扇门要薄。import 跳板别做成第二套运行时。

## 迁入

成熟后从 agenterm upsert 到本文件，不另起一篇把笔记冲掉。
