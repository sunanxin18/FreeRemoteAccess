# Linux 客户端补齐执行计划

承接已批准多平台 RDP 计划 Task 9；设计见
[Linux 客户端设计](../specs/2026-09-08-linux-client-design.md)。

- [x] 实现独立 Linux 平台服务 crate：XDG 路径、原子 profile/pin、TOFU、单实例锁、
  非回显环境 provider、Secret Service stage/commit/discard 与失败关闭。
- [ ] 用合成目录和注入 keyring 完成平台服务回归；Linux 原生临时 D-Bus/keyring
  往返单列，不把 macOS 上的 mock/Unix 文件测试称为 Secret Service 运行证明。
- [ ] 新增 Linux 应用组合根与依赖边界检查，默认 LegacyOnly，只有精确 bundle
  和 decoder 通过才允许显式 ValidationOnly 实验。
- [ ] 接入 Linux Vulkan/GLES 条件后端；在 X11/Wayland 宿主完成真实 GPU/输入与
  窗口装饰验收，保留 Windows DX12 与 macOS ARM64 Metal 行为。
- [ ] 实现完整 Linux stage/verifier（executable、desktop entry、图标、codec、许可证、
  真实 ELF 架构及 trusted-loader 布局）；不存在 native artifact 时失败，不跳过。
- [ ] CI 构建 Linux i686/x86_64/AArch64 客户端包并执行对应目标 fixture。
- [ ] 更新 README 客户端/服务器双轴状态，区分编译、包、真实 GUI/Secret Service
  与真实 RDP 控制，不用 FFmpeg bundle 冒充完整客户端。

范围顺序：平台服务 -> 组合根 -> Linux 窗口与GPU ->完整包/CI ->GUI与实机控制。
共享接口或文件按依赖顺序整合；协议、codec hot path 和平台窗口代码不混合。
Progressive coverage、实际 AVC 与 HEVC wire/live 门禁仍由原 RDP 计划独立推进。

2026-09-08 基础服务实现与三 Linux 目标 check 已完成，17 项合成平台测试通过；
Secret Service 原生脚本已加入 CI，未执行部分和完整客户端门禁见
[基础服务验证](../../validation/linux-client-foundation-20260908.md)。
