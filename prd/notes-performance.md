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

## 门

globals/locals 那扇门要薄。import 跳板别做成第二套运行时。

## 迁入

成熟后从 agenterm upsert 到本文件，不另起一篇把笔记冲掉。
