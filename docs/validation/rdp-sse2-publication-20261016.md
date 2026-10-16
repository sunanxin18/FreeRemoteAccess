# RDP RGBA→BGRX 的 SSE2 转换

2026-10-16，状态：**开发中**。本次补齐 Windows/Linux x86 与 x86_64 的 SSE2 转换主体；Linux x86_64已有新体局部实际运行；其余目标运行、保护页、ABI 和性能尚未完成，不能据此授予完整解码器或客户端生产资格。

RequiredNative 的 x86 准入要求 SSE2，原像素发布转换却只在 SSSE3 可用时使用原生实现，否则回到标量。现在保留 SSSE3 优先，其后检查真实 SSE2，再选择共享的 SSE2 入口；只有未通过 SSE2 检查的 CPU 才使用明确的 reference 后备。协议、解码器选择、Surface 合同、默认策略、能力广告和 ARM64 实现保持。

每个32位小端 lane 以移位、掩码和 OR 完成 `[R,G,B,A] → [B,G,R,255]`。完整16字节块和最后1–3个像素使用相同向量核心；尾部只搬运实际4/8/12字节，不读取调用方 padding。输入长度和目标容量在写入前检查，成功时也只修改目标对应前缀。

测试使用独立字节 oracle、literal RGB/opaque-X 锚点、40种像素数和16×16种源/目标偏移，共10,240例矩阵，并检查非法输入不写入和目标后缀保持。x86 测试只能否决真实 SSSE3，不能制造 SSE2 能力；实际 SSE2 入口的测试计数用于区分像素相同的标量后备。该计数仅在测试构建中存在，不能用它推断生产性能。

| 检查 | 当前结果与范围 |
|---|---|
| 固定 Rust 1.96 格式 | 两源通过 |
| macOS ARM64 局部真实源码测试 | debug/release各8通过；各过滤1项既有benchmark。仅共享API及NEON回归，不是x86执行 |
| Windows i686/x86_64 | 两个实际源码优化COFF对象编译通过，尚未链接或执行 |
| Linux i686/x86_64 | 两个实际源码优化ELF对象编译通过；后续普通CI已执行x86_64新体12项局部测试，i686仍待运行 |
| SSE2静态指令 | 四个精确入口及内联数值核心仅见SSE2指令；外部memcpy/TLS/panic运行库未链接，不代表最终ISA闭包 |
| 原体反例/新体执行 | 原体RED快照与精确运行方法已准备；Linux x86_64新体12项实际通过，其余执行仍待完成 |
| 保护页、ABI/unwind、有限性能 | 尚未完成；sentinel和对象编译不能替代 |

最终源SHA256：`pixel_convert.rs` 为`ee5623097b839166f311ffc0a9fffbfbc3d8b59163da96df8b9171692f4fa66c`，`pixel_convert/x86.rs` 为`232fabaca6781a9e3ef5110de292d3c4b017066b847f7ee9b31ad4f7ad8aeaf9`。原SSSE3数值体与ARM64文件保持；macOS只支持ARM64，本次没有macOS Intel构建。

报告和原始记录在`target/sse2-publication-implementation-20261016/`，root保留原两源在`target/sse2-publication-root-20261016/before/`。独立源码审查与后继真实运行结果分别记录，不把源码实现、对象编译、实际执行和性能混为一个通过状态。

独立源码审查结论为Spec/Quality PASS，未发现需返工问题；报告SHA256为`c8bf96bd718b7f5057862e4b0c36e0c9e094a21e449d8318daef58c867c9b66b`。root已完整读取两源diff、实现报告、审查报告，重新核对源快照及四目标对象身份。此结论仅接受源码切片，表中真实目标运行门继续开放。

Windows、Linux包流程已分别补上仅i686执行的实际crate局部测试接线，使用原目标/linker/runtime，跳过既有benchmark，失败即停止。两处独立配置审查通过（报告SHA256 `4208413f73f020396eed668867246d67a611e430403c71452d214b6e06cef6bc`）；当前只是接线，尚未取得该版本的CI执行结果。x64沿用现有完整crate测试，不新增重复运行。过滤器须实际运行11项局部测试（含三个ISA测试），不能用0测试的成功退出当验证。

## 首次 Linux x86_64 真实运行

提交`ba6035752f746651681708c07cefa1c896248880`的[普通CI38847485935](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38847485935)中，Ubuntu24.04 job116801271211成功；2026-10-16 03:54:47 UTC原日志记录该模块12项实际测试均为ok，包含三个ISA测试和既有benchmark。关键`publication_actual_sse2_dispatch_when_ssse3_denied`、直接SSE2/否决SSSE3矩阵和SSSE3优先恢复均实际执行通过，不是对象编译或空过滤。完整原job日志在`target/sse2-publication-root-20261016/ci/ba603575-linux-x64-job-original.log`。

这将Linux x86_64的新体局部正确性从“仅编译”推进到实际GREEN。旧体实际RED、i686与Windows对应运行、保护页、ABI/unwind、最终链接运行库ISA闭包及三路性能仍待完成；旧benchmark显示ok不等于本次新增SSE2路径已有速度收益。该CI的Mac ARM64及格式任务同样成功，不能外推其他架构或真实远程会话。

独立执行读数复核报告SHA256为`bbe881fe8f14c3e209d5d956c8e577b99468130ec69abce25e49db6d962aac84`，核对原API的head/job/runner、原日志中的1.96工具链/x86_64 host和逐名12条ok；不追加CPUID硬件清单或速度结论。
