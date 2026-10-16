# HEVC 32×32 angular：原版参考与分派基线

2026-10-16，已批准 Task4C5 的测试前置步骤。两份新 C 测试经过独立源码审查；Mac ARM64 私有原版/model 与真实 post0002 分派基线已实际执行。此时尚无新32汇编实现，生产 codec 广告及默认策略保持。

使用未改的 FFmpeg 8.1.2 pred.c include-TU（仅改初始化器名称），与独立的逐像素 floor/参考投影模型对照。实际初始化器来自此前已接受的 post0002 pred/init/NEON 三对象，不拿原版 initializer 代替有旧16优化的真实配置。旧16对象、原版树及 avutil 保持只读。

- 原版/model/实际 C 槽共 118404 cases、236808 callback 调用通过；每次核完整8192B输出及两份128B输入。
- 覆盖46图案、模式2..34、c_idx0/1/2。数值4554、三图案的独立地址布局109296、同址引用4554；布局扫8种正负stride和三个地址各mod16，未声称三地址16³或全图案全布局交叉。
- CPU off/on与8/9/10/12/0/7/16完整context通过，旧16两个精确hook保持，未知深度保持原C表。
- 独立RED程序实际 exit3，精确确认新angular32 wrapper未被选择，且旧16及其它context均正常。编译、链接均exit0；没有把缺符号、崩溃或其它非零当RED。

Apple clang21/ARM64，绑定原 dist avutil SHA256 `29b18c1c99fe94fbb369191ab3b4602d671f0903019f3bf520a948c193c21ac6`；dyld日志确认实际装载。27项源码/配置/复用对象/依赖/编译器身份前后相同。十个命令的argv、退出和原始流保存在 `target/hevc32-reference-root-20261016/`，results.json SHA256 `d6a62d50f2110a0d59c6cd43207f7578e7f49f7f4a32628afe8dc61d4b88c702`。

新native测试只准备未来wrapper全模式数值、直接垂直数值及最小参考读取保护页；均未执行，不是新核、ABI、性能或目标平台资格。没有重跑旧16门、重新构建整个FFmpeg、修改0002或运行设备/模拟器。后续须独立验证新NEON、实际selector GREEN、最小读取、直接ABI及最终指令，之后再分别验证性能、其它平台、正式配方/包和完整同流解码。

独立执行数据复核通过：报告SHA256 `bcd95c1a77da5446a900ce4d7ecb32a763f07ff3b2382c4796270382b1bc8ccf`；root全文读并重核绑定。该复核未重复运行测试。原版pred.c/header另通过执行前源码审查收据与当前哈希补充核对，未将其冒充27项before/after原始集合内条目。
