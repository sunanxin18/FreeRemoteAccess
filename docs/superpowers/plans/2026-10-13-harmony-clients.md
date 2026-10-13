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
- Task 1–3 实现及修复已通过独立审查，当前进入 Task 4；准备结果不将任何 HAP、图形、协议互操作任务标为完成，各步骤在实现及审查后单独勾选。

### Task 1: OHOS 工具链与共享执行器边界

**Files:** 新增 `tools/build-harmony.sh`、`crates/frd-shell-runtime/Cargo.toml` / `src/lib.rs`；移动 desktop 的 `session_host.rs`、`cleanup.rs`、`video_decode_worker.rs` 及必要 metrics 模块；修改 workspace、desktop manifest/re-exports。

**Interfaces:** desktop 薄 wrapper 保留 `SessionHost` 三参数构造与既有公共行为；新 runtime 的 `SessionHost::new` 显式接收第四个 decoder registry loader 参数。`WakeSink`、`AcceptedLaunchOutcome` 等契约保持。中立 worker 不自行加载 decoder，desktop composition root 保留原 FFmpeg 选择；中立的 `video_rate_fallback` 随 worker 迁移以保留原交叉测试。

- [x] 在迁移前记录 desktop/session 测试基线，依赖检查验证新 runtime 无 winit、egui、GTK。
- [x] 无语义变更提取原实现；先跑迁移后的原测试，再检查 OHOS target 的核心/runtime 构建。
- [x] 配置 SDK OHOS Clang/sysroot，不硬编码本机路径进源码；依赖构建失败按具体库处理。
- [x] 运行 `cargo test --locked -p frd-shell-runtime -p frd-shell-desktop` 与 OHOS target check，记录 host/target 层级。

Task 1 于 `655785d` 完成并经独立审查通过；各架构验证范围见[环境与运行时验证](../../validation/harmony-environment-20261013.md)。

### Task 2: 原生 HAP、C ABI 与 XComponent 呈现

**Files:** 新增 `apps/freeremotedesk-ohos/` ArkUI/Hvigor/entry/native 工程，`crates/frd-shell-ohos-ffi/` 与 `include/frd_ohos.h`。

**Interfaces:** `frd_ohos_abi_version() -> u32`，`frd_ohos_create() -> u64`，`frd_ohos_destroy(engine: u64) -> i32`；`frd_ohos_attach(engine, window, width, height, out_surface_generation: *mut u64) -> i32`、`frd_ohos_resize(engine, surface_generation, width, height) -> i32`、`frd_ohos_detach(engine, surface_generation) -> i32`。engine 是 Rust registry 分配的不复用整数句柄，0 为无效值；surface generation 同样单调递增，迟到回调不可拆除或改变新 surface。不得解引用已释放的 Rust 对象指针来判断有效性。只有 `window` 是调用方保证生命周期的 native 指针，attach 成功前取得独立引用。native bridge 不解析协议。

- [x] 测试空/失效句柄、重复 detach、零尺寸、迟到回调和销毁顺序；错误均可识别且不得 unwind。
- [x] 实现 owned OHNativeWindow lease 与 compositor，呈现确定性图案仅作为图形 POC，产品登录页不伪装网络连接。
- [x] 构建 phone/2in1 HAP，核验 lib 架构、资源、权限与签名；通过 HDC 安装到独立实例。
- [x] 在两个模拟器验证 attach/resize/detach、前后台和画面呈现；PC 缺镜像自动下载后验证，不用手机布局冒充 PC。

Task 2 于 `822c359` 实现、`2636a6c` 修复并通过独立复审。两台图形、手机横竖屏、PC 最大化/缩放与前后台通过，完整产品安全区等仍属 Task 3；见[图形验证](../../validation/harmony-hap-20261013.md)。

### Task 3: ArkUI 行为与鸿蒙平台服务

**Files:** `crates/frd-platform-ohos/`；OHOS ArkUI 页面、native N-API bridge 与 HUKS adapter。

**Interfaces:** 实现现有 `SecureCredentialStore`、`ConnectionProfileStore`、`ServerIdentityStore`；复用 `AppController` / `AppIntent` / `ConnectionForm`。C ABI 快照仅含可复制非秘密状态，连接请求中的密码按调用期借用并立即转入 SecretBuffer。

- [x] 覆盖 stale credential ticket、认证成功后才commit、指纹变化拒绝与未知能力不可用。
- [x] 实现 phone/PC 响应式表单和状态意图，HUKS保存/读取、密码字段 IME 提交与 native committed-text 归一化、触控/键鼠归一化。真实远程软键盘入口和端到端 IME 随 Task 4 的实际会话视图完成，不由本任务 ABI 测试代替。
- [x] 验证横竖屏、PC窗口缩放、明暗主题、安全区、普通设置最大字体缩放、密码字段焦点及安全键盘；ReleaseAll 有确定性回归。完整 Tab 遍历/PC 滚轮、真实会话输入与窄窗口 chrome 保留为后续实际会话验收，不由 ABI 单测替代。

Task 3 于 `c908733` 实现、`3b59cef` 修复，六项 P2 独立复审通过；两台模拟器 HUKS、实际字体及安全键盘复验范围见[应用与安全存储验证](../../validation/harmony-app-services-20261013.md)。生产协议目录仍为空，不代表远程登录完成。

### Task 4: Windows RDP 实际连接

**Files:** OHOS composition root、桥接 session/state、平台 codec bundle builder/verifier。

**Interfaces:** 注册 `RdpProtocolFactory`（wire identity Unix），沿用 TOFU；FrameMailbox/FrameTransactionCompiler → renderer。UI 只有真实完整基线呈现后进入交互状态。

- [x] Task 4A1 独立 RemoteFX checked NEON/SSE2 数值叶子及审查完成（`c3ae798`）；尚未接入生产，模拟器基准未证明提速。
- [x] Task 4A2.1 packed Bitmap 五类 checked SIMD 叶子完成（`563de87`），独立审查通过，宿主及后续 OHOS 正确性/基准、八目标对象生成；完整 parser/session 仍待后续。
- [x] Task 4A2.2 RDP6 planar 数值叶子完成（`9fe010b`），独立审查通过，固定工具链宿主及手机/PC OHOS 各 25 项回归；两台叶子基准保留更快/更慢差异，不代表完整 RDP6 接线。
- [ ] Task 4A2.3 先隔离原版 oracle workspace/process 与开发依赖，再固定最小 graphics/session 生产副本；通过依赖图审查后依次推进完整 Bitmap、RDP6、RLGR/RFX 和图像转换接线。
- [x] Task 4B1 真实事务→原生呈现、统一视口与输入门接线完成（`87c829a`/`bccb321`），两台合成首帧、触点及本地重绘受限验证，跳帧重试修复经独立复审；不代表真实协议连接，见[呈现验证](../../validation/harmony-native-presentation-20261013.md)。
- [x] Task 4C0 手机/PC 正常权限系统能力与三个接口路线完成受限验证；AVC420 像素通过、EOS 前出帧未通过、精确444失败、HEVC 未报告，见[系统验证](../../validation/harmony-system-codec-20261013.md)。
- [x] Task 4C1 中立 NativeFirst 显式策略 `e1fb9fa` 已实现并通过独立审查；默认桌面顺序保持，尚未接入系统 backend。
- [x] Task 4C2 官方基础镜像静态审计找到双线程缓冲路径及安装 NEON 分派；与三轮运行结果形成高置信度原因解释，未证明当前进程加载身份。当前系统软解不满足静态单帧要求，优先推进合格 FFmpeg 后备，系统优先策略保留。
- [x] Task 4C3A 独立官方 OHOS ARM64 FFmpeg builder/verifier 与专属静态输出线程策略完成（`9c2cd42`/`8343bd3`）；22 项产物/输入/实际链接回归通过，独立复审通过。仅构建证据，普通 HAP loader、逐像素与运行时性能仍待完成，见[后备包证据](../../validation/harmony-ffmpeg-bundle-20261013.md)。
- [x] Task 4C3B 应用自身模块锚定加载器完成（`4f5089e`），47 项宿主测试、OHOS ARM64 构建与公开 NDK 链接通过，独立审查通过；普通 HAP 安装路径/权限与实际加载仍待验证。
- [x] Task 4C3D 普通 HAP 包装与签前/签后字节校验完成（`22513b2`/`dc9419b`），38 项范围测试及独立复审通过；同一最终签名 Debug 包已在手机/PC 安装启动。应用进程内加载和解码仍待 Task 4C3E，包中存在库不代表 DecoderReady。
- [x] Task 4C3E 正常应用内真实 FFmpeg 解码夹具完成（`6a2c91c`），手机/PC 同一签名包各 13/13；无 EOS 首/末帧、独立像素、1440p/4K 合成帧与生命周期通过，OHOS 目录句柄修正保留检查，38 项 FFI/49 项 loader 测试及独立审查通过。未注册生产 codec，运行时 SIMD、持续吞吐与真实远程会话仍待验证。
- [x] Task 4C4 原三库优化应用测量及分格式运行审计完成（`f70b381`），两台基准各 10/10、AVC444/HEVC444 指令级正向证据，独立代码审查通过；夹具再生成脚本小项由 `c9f5217` 修正并通过临时目录完整比较。实际仍存在计算标量分支，不代表生产资格。
- [x] Task 4C5A 单个 AArch64 8-bit H264 luma DC 内核完成（`93e0a6b`），原 C/数值/内存对照 5,974,518 组、30 种 dispatch、checkasm 30 项及本机有界微基准通过，独立审查无问题；只属于 macOS ARM64 私有构建，源码 recipe、OHOS 包与手机/PC实际执行仍待完成，见[内核验证](../../validation/h264-luma-dc-neon-20261013.md)。
- [ ] Task 4C5 优先补 AArch64 8-bit H264 luma DC 变换/反量化；维护显式补丁来源、C oracle、逐核 parity、目标基准和普通 HAP 同流验证。HEVC 16×16 planar/angular 与已识别的 32×32 angular 标量分支随后处理，避免重写已向量化分支。
- [ ] 优先完成 AVCodec Kit 逐设备能力与精确 fixture 验证、系统硬解/软解 adapter 和生命周期接线；软解保留 SIMD 证据要求。系统后端未覆盖的格式再使用经过 OHOS ABI/parity 验证的 FFmpeg 后备。
- [ ] 确认 ARM64 Bitmap/RemoteFX/像素转换及生产 dispatch 满足现有 SIMD gate；独立叶子不能替代完整解码路径。
- [ ] 模拟器连接授权 Windows：保存/读取凭据、首帧、增量、触控/键鼠、远程软键盘入口与 IME 提交、断开/重连与恢复。
- [ ] AVC/HEVC广告继续受原精确门禁控制；未通过的 codec 不由新平台构建自动开放。

### Task 5: Mac 实际连接与平台媒体

**Files:** OHOS composition root、native media backend、包验证脚本。

**Interfaces:** 注册现有 Apple factory；平台 codec 注入既有 `VideoDecoderRegistry`，不修改Apple wire或在Mac安装程序。

- [ ] OHOS 系统 H.264/HEVC 和音频能力优先按既有 media 契约验证精确格式/生命周期；AAC MIME 不代表 AAC-ELD。未覆盖路径保留 fdk-aac/FFmpeg 后备并完成真实交叉构建及目标执行。
- [ ] 授权Mac完成真实认证、首帧、持续更新、输入、重连；音频/剪贴板只报告已实现并验证的能力。
- [ ] 跑Windows/macOS/Linux相关回归及包检查，记录本轮没有真机的平台。

### Task 6: 鸿蒙手机系统原生服务证据与交付矩阵

**Files:** 新增 `docs/validation/harmony-native-service-20261013.md`；更新 `README.md` 客户端/服务端两轴矩阵。

- [x] 核对官方API、系统原生服务入口及模拟器实际可用服务；不采集其他应用秘密、不使用调试注入实现产品功能。
- [x] 有可验证协议才新增独立 adapter，先确定性fixture再真实互操作；无服务/权限证据则明确具体缺口，不造假可用目标。
- [ ] 记录每个HAP/架构/模拟器/协议的版本和验证范围；保留真实未完成项。
- [ ] 完成独立最终审查、格式、共享测试和受影响平台构建；commit/push按用户交付授权执行。
