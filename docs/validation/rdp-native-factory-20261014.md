# RDP 显式原生 legacy 策略与目标接线

2026-10-14。`2015400` 添加 `RdpLegacyDecodePolicy::{Compatibility, RequiredNative}` 和显式工厂 builder，经独立审查 Approved。现有构造函数仍默认 Compatibility，不探测原生后端；RequiredNative 在配置校验、会话及网络执行前只探测一次实际 CPU 后端，不可用时返回静态错误，不回退 scalar。选择通过实际 factory/session/runtime 传到初始 ActiveStage，并由既有 setter 在重建 Fast-Path processor 后保留。客户端 wire 身份不决定实际 ISA，未改变 codec 广告、EGFX 门、桌面默认策略或 OHOS 协议目录。

## 源码与宿主验证

六个 Rust 文件的独立审查覆盖实际调用链、原生失败与 Compatibility 的可观察行为、重激活和新 generation 内容清空。固定 Rust 1.96 宿主受影响套件 413 项通过、3 项既有 ignored；release Stage 41 项及 factory 10 项通过。删除真实 Stage 绑定、丢弃会话选择、不可用时回退 Compatibility 的三个已编译变体均被语义断言检出。既有非本范围警告仍保留。

## OHOS 手机与 PC 实际执行

Root 对完整 `20154004da8ea84312e124cb44051ca5a0f32297` 生成不可变源码归档，SHA256 为 `3707eeaa97beda56bd63e518bcd9680ef3162e6d8ed6e54e2565309bbac8f3ee`，六个受审源码文件与归档逐一匹配。使用官方 API26 NDK 和实际 Rust 1.96、offline/locked 构建 RDP crate 的测试程序，45.48 秒完成。

Cargo JSON 唯一选中的库测试 ELF 为 ELF64 AArch64，仅依赖 `libc.so`；SHA256 为 `8ad8fb70125b9cc7b93ef2fe2c7688ed729d0b7fc53c5b2ad4a9a6e508d3a835`。两台独立 API26 ARM64 模拟器分别发送并核对相同 ELF：

| 实际测试范围 | 手机 | PC |
|---|---|---|
| `legacy_native` 工厂与实际 Stage 接线、失败、替换处理器、新 generation | 6/6 | 6/6 |
| `decoded_coverage_` 经真实 factory 选择的编码内容、完整首帧、队列恢复与跨 generation 门 | 20/20 | 20/20 |

四组均为真实测试进程 `FRD_EXIT=0`，没有失败；不把 HDC 外层成功当测试成功。日志、源码/ELF 哈希、动态依赖和两端结果位于 `target/harmony-native-factory-20261014/`。此前未改的 17 项 baseline 测试保留 `fa825f1` 的两端证据，本次没有为扩大计数重复运行。

## 剩余边界

这是显式策略与目标接线验证，不是完整软件解码资格。完整编码输入到 image、Surface、mailbox 和事务编译的性能、软件 cursor 像素热路径、其它架构与包仍须独立完成。OHOS 普通 HAP 未启用生产协议目录，本次没有替换 HAP、恢复开发验证入口、使用远端凭据或执行真实登录。Windows/Linux/macOS 的既有默认策略保持，macOS 仍仅支持 ARM64。
