# Linux 客户端补齐执行计划

承接已批准多平台 RDP 计划 Task 9；设计见
[Linux 客户端设计](../specs/2026-09-08-linux-client-design.md)。

- [x] 实现独立 Linux 平台服务 crate：XDG 路径、原子 profile/pin、TOFU、单实例锁、
  非回显环境 provider、Secret Service stage/commit/discard 与失败关闭。
- [x] 用合成目录和注入 keyring 完成平台服务回归；Linux 原生临时 D-Bus/keyring
  往返单列，不把 macOS 上的 mock/Unix 文件测试称为 Secret Service 运行证明。
- [x] 新增 Linux 应用组合根与依赖边界检查，默认 LegacyOnly，只有精确 bundle
  和 decoder 通过才允许显式 ValidationOnly 实验。
- [ ] 接入 Linux Vulkan/GLES 条件后端；在 X11/Wayland 宿主完成真实 GPU/输入与
  窗口装饰验收，保留 Windows DX12 与 macOS ARM64 Metal 行为。
- [x] 实现完整 Linux stage/verifier（executable、desktop entry、图标、codec、许可证、
  真实 ELF 架构及 trusted-loader 布局）；不存在 native artifact 时失败，不跳过。
- [x] CI 构建 Linux i686/x86_64/AArch64 客户端包并执行对应目标 fixture。
- [x] 更新 README 客户端/服务器双轴状态，区分编译、包、真实 GUI/Secret Service
  与真实 RDP 控制，不用 FFmpeg bundle 冒充完整客户端。

范围顺序：平台服务 -> 组合根 -> Linux 窗口与GPU ->完整包/CI ->GUI与实机控制。
共享接口或文件按依赖顺序整合；协议、codec hot path 和平台窗口代码不混合。
Progressive coverage、实际 AVC 与 HEVC wire/live 门禁仍由原 RDP 计划独立推进。

## 当前验证边界（2026-09-08）

- 平台服务：17 项合成回归和三 Linux 目标 check 通过；`5cd73ea` Ubuntu job `101972000994` 的隔离 Secret Service 真实往返 1 项明确执行通过，原生 Linux 应用 20+2 项测试通过。
- 完整包：`5cd73ea` i686 job `101971932014` 完成应用测试、release 构建、包校验、实际解码器加载及产物上传；ARM64/x86_64 同轮在 zlib 系统依赖名单处失败。a60eebd 的 ARM64/x86_64 修正后 jobs 101983147418/101983147612 已通过完整包及目标解码加载，同轮 i686 job 101983147659 随后也完成应用测试、完整包及目标解码加载，run 34202134581 三目标全部通过。
- 原生窗口：GTK4 HeaderBar/GLArea 独立技术探针和三架构 X11/Wayland 1×/2× CI 已实现；X11 驱动使用独立 Xvfb 实际点击/F8，Wayland只检查 GL/几何。首轮标题栏边界、Wayland 输出 scale 和日志污染失败均保留。`34233406400`（`af77f46`）三架构 X11/Wayland 1×/2× 共12份报告通过 frame、runner、submission、observer gap；runner 还断言 `font_map_loaded=true` 并生成中文可见的离屏登录/连接 PNG。GTK 输入控制器已接线，但 fixture 使用合成 GTK 事件，不构成物理输入或真实 RDP 控制证明。当前正式入口已切换 GTK Application/ApplicationWindow（app-id `com.sunanxin18.freeremotedesk`），并新增 X11 `tools/verify-linux-product-x11.sh`（WM_CLASS、焦点、XTEST 键鼠、清理退出）和 Wayland `tools/verify-linux-product-wayland.sh`（xdg_toplevel app-id、正常 close-request 清理）；两脚本尚待本轮三架构 CI 执行记录，Wayland 全局物理输入仍需在授权桌面设备上单列验收。
- 平台隔离：后续 GTK 窗口集成复用 AppController 的登录意图和 SessionHost 的启动/取消/清理。已将无平台依赖的 SessionHost 从 application 模块分离，保留公开 API 和行为，完整工作区1706项通过；不要复制登录流程或把 GTK 类型引入协议、codec、SurfaceUpdate。
- 共享帧事务：已迁移至独立 frd-render-state crate，独占候选只允许消费提交一次；旧38项测试全部保留为纯状态10项和Metal后端28项，新增3项候选回归及7项编译拒绝测试通过。独立 Linux GL 执行器已实现纹理上传、绘制和上下文生命周期，三目标编译检查通过；c349a72 run34210588972 三架构软件 EGL 已通过两种颜色输出契约及生命周期 fixture。frd-shell-gtk 已接入真实 FrameTransaction→GLArea 绘制，`34233406400` 三架构 X11/Wayland 1×/2× 共12个原生 fixture 通过完整帧、增量、上下文重建、登录/取消、窗口提交和字体 family 门禁；Linux入口切换、物理输入和完整产品接线仍未完成。
- Windows/macOS：继续保持原有平台入口、渲染后端和会话行为。主机编译或离线目标测试均不替代真实 GUI 控制。

完整证据与失败记录见[基础服务与客户端验证](../../validation/linux-client-foundation-20260908.md)。下一执行顺序：Linux Vulkan/GLES 条件后端 -> X11/Wayland 物理输入与窗口装饰验收 -> Linux 产品入口和真实 RDP 控制。包修正 CI 与窗口实现可独立推进；实际 Progressive、AVC、HEVC 的原始门禁继续保留。
