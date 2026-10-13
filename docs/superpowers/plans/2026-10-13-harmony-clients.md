# 鸿蒙手机与 PC 客户端 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付可构建、可在手机和 PC 模拟器运行的原生客户端，复用现有 Windows/Mac 远程协议，并以系统原生服务证据单独判定鸿蒙手机被控能力。

**Architecture:** ArkUI/XComponent → 版本化 C ABI → 共享 app/session/frame/runtime。平台服务与 codec backend 独立；不修改已有协议的 wire 行为，不复制 desktop 会话状态机。

**Tech Stack:** Rust 1.96、ArkTS/ArkUI、DevEco API 26、Hvigor 6.26.4、CMake、OHOS ARM64、pinned wgpu GLES。

**Spec:** `docs/plans/2026-10-13-harmony-clients-design.md`

## Global Constraints

- 所有被控设备只使用系统原生服务；鸿蒙手机禁止配套被控端或 HDC 产品链路。
- ArkTS 不传帧像素；密码不进入快照/日志/普通配置。
- Windows/Linux/macOS 既有功能不回退；macOS 只支持 ARM64。
- 生产能力按精确协议/解码/平台/包/互操作证据广告；模拟器成功不等于真机完成。
- 软件解码热路径使用汇编/SIMD；OHOS 不误入桌面 Linux 分支。

## Review Focus

- 旋转/后台后迟到的 native 回调不得访问已释放窗口或发送旧 generation 输入（Task 2）。
- 用户输入新密码后旧 HUKS 结果不得覆盖，取消后不得保存失败登录的密码（Task 3）。
- 首个部分帧和溢出恢复不能被误记为完整可交互画面（Task 4）。
- OHOS 的 Linux cfg、FFmpeg 包路径与桌面运行时依赖不能隐式混用（Tasks 1/4/5）。
- API 存在、模拟器调试注入和画面演示不能被算成鸿蒙手机系统原生远控（Task 6）。

## 执行前准备记录（2026-10-13）

- 官方 PC ARM64 镜像已下载，独立 phone/2in1 模拟器均启动且 HDC Connected；Rust 1.96 OHOS std 已安装。
- 源码未改动的 desktop/session 基线共 228 项测试通过；现有 core/frame/app 的 OHOS target check 通过。
- [环境证据](../../validation/harmony-environment-20261013.md)与[鸿蒙手机原生服务调查](../../validation/harmony-native-service-20261013.md)已记录。
- 下列产品实现任务尚未开始；这些准备结果不将任何 HAP、图形、协议互操作任务标为完成。

### Task 1: OHOS 工具链与共享执行器边界

**Files:** 新增 `tools/build-harmony.sh`、`crates/frd-shell-runtime/Cargo.toml` / `src/lib.rs`；移动 desktop 的 `session_host.rs`、`cleanup.rs`、`video_decode_worker.rs` 及必要 metrics 模块；修改 workspace、desktop manifest/re-exports。

**Interfaces:** 保留 `SessionHost`、`WakeSink`、`AcceptedLaunchOutcome` 的既有签名；正式提供 video registry 注入，desktop composition root 保留原 FFmpeg 选择。

- [ ] 在迁移前记录 desktop/session 测试基线，依赖检查验证新 runtime 无 winit、egui、GTK。
- [ ] 无语义变更提取原实现；先跑迁移后的原测试，再检查 OHOS target 的核心/runtime 构建。
- [ ] 配置 SDK OHOS Clang/sysroot，不硬编码本机路径进源码；依赖构建失败按具体库处理。
- [ ] 运行 `cargo test --locked -p frd-shell-runtime -p frd-shell-desktop` 与 OHOS target check，记录 host/target 层级。

### Task 2: 原生 HAP、C ABI 与 XComponent 呈现

**Files:** 新增 `apps/freeremotedesk-ohos/` ArkUI/Hvigor/entry/native 工程，`crates/frd-shell-ohos-ffi/` 与 `include/frd_ohos.h`。

**Interfaces:** `frd_ohos_abi_version() -> u32`，`frd_ohos_create() -> u64`，`frd_ohos_destroy(engine: u64) -> i32`；`frd_ohos_attach(engine, window, width, height) -> i32`、`frd_ohos_resize(engine,width,height) -> i32`、`frd_ohos_detach(engine) -> i32`。engine 是 Rust registry 分配的不复用整数句柄，0 为无效值；不把已释放的 Rust 对象指针解引用来判断有效性。只有 `window` 是调用方保证生命周期的 native 指针，attach 成功前取得独立引用。native bridge 不解析协议。

- [ ] 测试空/失效句柄、重复 detach、零尺寸、迟到回调和销毁顺序；错误均可识别且不得 unwind。
- [ ] 实现 owned OHNativeWindow lease 与 compositor，呈现确定性图案仅作为图形 POC，产品登录页不伪装网络连接。
- [ ] 构建 phone/2in1 HAP，核验 lib 架构、资源、权限与签名；通过 HDC 安装到独立实例。
- [ ] 在两个模拟器验证 attach/resize/detach、前后台和画面呈现；PC 缺镜像自动下载后验证，不用手机布局冒充 PC。

### Task 3: ArkUI 行为与鸿蒙平台服务

**Files:** `crates/frd-platform-ohos/`；OHOS ArkUI 页面、native N-API bridge 与 HUKS adapter。

**Interfaces:** 实现现有 `SecureCredentialStore`、`ConnectionProfileStore`、`ServerIdentityStore`；复用 `AppController` / `AppIntent` / `ConnectionForm`。C ABI 快照仅含可复制非秘密状态，连接请求中的密码按调用期借用并立即转入 SecretBuffer。

- [ ] 覆盖 stale credential ticket、认证成功后才commit、指纹变化拒绝与未知能力不可用。
- [ ] 实现 phone/PC 响应式表单和状态意图，HUKS保存/读取、IME commit、触控/键鼠归一化。
- [ ] 验证横竖屏、PC窗口缩放、明暗主题、安全区、字体缩放、键盘焦点和 ReleaseAll。

### Task 4: Windows RDP 实际连接

**Files:** OHOS composition root、桥接 session/state、平台 codec bundle builder/verifier。

**Interfaces:** 注册 `RdpProtocolFactory`（wire identity Unix），沿用 TOFU；FrameMailbox/FrameTransactionCompiler → renderer。UI 只有真实完整基线呈现后进入交互状态。

- [ ] 确认 ARM64 Bitmap/RemoteFX/像素转换满足现有 SIMD gate；FFmpeg OHOS 原生构建及 ABI/parity 独立验证。
- [ ] 模拟器连接授权 Windows：保存/读取凭据、首帧、增量、触控/键鼠、断开/重连与恢复。
- [ ] AVC/HEVC广告继续受原精确门禁控制；未通过的 codec 不由新平台构建自动开放。

### Task 5: Mac 实际连接与平台媒体

**Files:** OHOS composition root、native media backend、包验证脚本。

**Interfaces:** 注册现有 Apple factory；平台 codec 注入既有 `VideoDecoderRegistry`，不修改Apple wire或在Mac安装程序。

- [ ] OHOS fdk-aac/native依赖真实交叉构建，H.264/HEVC backend在既有media契约下验证生命周期/格式。
- [ ] 授权Mac完成真实认证、首帧、持续更新、输入、重连；音频/剪贴板只报告已实现并验证的能力。
- [ ] 跑Windows/macOS/Linux相关回归及包检查，记录本轮没有真机的平台。

### Task 6: 鸿蒙手机系统原生服务证据与交付矩阵

**Files:** 新增 `docs/validation/harmony-native-service-20261013.md`；更新 `README.md` 客户端/服务端两轴矩阵。

- [ ] 核对官方API、系统原生服务入口及模拟器实际可用服务；不采集其他应用秘密、不使用调试注入实现产品功能。
- [ ] 有可验证协议才新增独立 adapter，先确定性fixture再真实互操作；无服务/权限证据则明确具体缺口，不造假可用目标。
- [ ] 记录每个HAP/架构/模拟器/协议的版本和验证范围；保留真实未完成项。
- [ ] 完成独立最终审查、格式、共享测试和受影响平台构建；commit/push按用户交付授权执行。
