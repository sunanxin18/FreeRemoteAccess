# FFmpeg 私有平台接口验证

2026-10-14。本次把 H.264 luma DC 私有数值测试的保护页和时钟操作提取为独立平台接口，为后续 Windows ARM64 验收复用同一套数学用例。当前只实现并执行 POSIX 分支；Windows 分支仅有声明。

共享 C 的数值公式、原版 oracle、随机种子、5,974,518 组用例、30 种分派、输入/输出布局、ABI 探针和基准循环均保持。生产 FFmpeg patch、配方、对象、包、默认选择和协议能力没有变更。平台层不进入协议或 UI。

## 实际结果

Apple clang 21.0.0、macOS ARM64：独立平台契约编译及执行成功，覆盖零/脏 owner、页大小与乘法溢出、映射/保护/释放失败、失败后所有权、只读输入、两侧保护页和单调时钟的失败输出保护。两份刻意削弱的实现均编译成功，并分别在 `input start guard` 与 `read-only input write` 明确失败、exit 1；编译失败或任意崩溃未计为负例通过。

复用原 macOS 配方的六个对象及同一份 avutil dylib，新私有程序完整执行成功：`PASS parity cases=5974518`，30 contexts、119 vectors、13260 QP/scaling、200000 random、guard/RO 与 AArch64 ABI sentinels 保持。未运行 benchmark，未构建第二份 FFmpeg。七个 shipping 输入、配置、原 oracle 和本次源码前后哈希一致。

原始命令、工具身份、实际进程 PID/退出、输出及哈希在 `target/task4c5c3b1-private-platform-posix-20261014/execution.json`；六条数值构建/执行命令与 fixture 前后闭包在其 `numerics/results.json`。根代理已独立复核实际输出、终态、七个输入及源码闭包。独立源码与执行证据复审均通过（Spec/Quality PASS）；未发现需要修复的事项。

## 后续门

本次 POSIX 结果不能证明 Windows adapter、VirtualAlloc/Protect/Free、QPC、ARM64 COFF/SEH/原生 unwind、Windows 数值/基准、最终修改包或真实 RDP 已完成。Linux/OHOS 新平台接口的本轮执行未开展，旧结果仍按原文件身份保留。HEVC 原版参考与后续汇编仍分别验收。完整 codec 和普通鸿蒙远程登录保持开发中，NativeFirst 和桌面默认保持。
