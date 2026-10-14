# HEVC 16×16 原版参考与分派前置验证

2026-10-14。FFmpeg 8.1.2、macOS ARM64。本次建立真正未修改的 HEVC 原版 C 回调、独立整数 floor 模型及新 NEON 核实现前的精确分派负例。没有实现新汇编、修改配方或开放生产 codec。

独立原版 TU 直接 include 固定 pred.c/pred_template.c，仅改名 initializer；另链接既有实际 pred.o 和同一份 dist avutil。正常程序实际执行 119600 cases，覆盖 planar、angular mode2..34×c_idx0/1/2、46图案、八组含负值的 stride、各指针mod16与完整buffer/input比较。完整context检查涵盖8/9/10/12/0/7/16×CPU off/on。

单独 RED 程序编译与链接成功，在原版 init 保持C、两个待新增NEON符号未被选择时，输出精确selector文字并 exit3；不是把编译、链接或任意崩溃当作负例。guard宏形态也实际 exit0，固定循环额外4800 cases/9600 callbacks；日志仍只打印常规119600，额外数字来自冻结循环库存，未混计。planar专用最小读取范围、ABI与性能仍需后续验收。

三程序 dyld 实际加载同一份 dist libavutil.60.26.102.dylib，SHA256 `29b18c1c99fe94fbb369191ab3b4602d671f0903019f3bf520a948c193c21ac6`。15项源码/配置/对象/库/编译器前后及当前身份一致；39项输出身份一致。原始 argv、PID、终态、依赖与加载日志在 `target/task4c5d1a-reference/attempt-rwm9ndal/results.json`（SHA256 `cab2524c7318e16875b336feb21fdbe2ee927c94c82ee0a067b4dfdb8c0e305e`）。根代理独立复核，源码与执行证据独立审查均为 Spec/Quality PASS。

实际门允许进入16×16 planar/angular两个NEON核的实现，不能证明NEON GREEN、跨容器ABI、性能、组合配方、最终HAP、手机/PC运行或真实远程会话。生产选择与系统NativeFirst不变，HEVC完整资格继续开发中。
