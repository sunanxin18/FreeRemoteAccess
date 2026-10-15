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
- [x] Task 4A2.3 原版 oracle workspace/process 与开发依赖隔离、固定 graphics/session 生产副本完成（`1455c2d`），前后依赖图及独立审查通过；保留22,337条完整wire记录和原数值覆盖，宿主正确性/来源复算/RDP321项通过，生产decoder源码未改。
- [x] Task 4A2.4 `484d1b2` Bitmap 共享 parser 原行为保持，显式 native checked 叶子接入；16721冻结原版及7616畸形case、121856canary运行通过，独立审查 Approved。范围为host ARM64，session尚未切换。
- [x] Task 4A2.5 `0f207c8` RDP6共享parser/controller原生接线与独立审查通过；6906普通bitmap、2237privateplane与16geometry安全classification，宿主debug/release各20项、冻结实际OHOS库手机/PC各20项通过。当前仅显式consumer，session默认未切。
- [x] Task 4A2.6 `23ea916` 共享RLGR真实零填充、checked RFX组件与显式会话接线，独立审查Approved；宿主core3/session4 debug/release、6故障变体及既有回归通过，root冻结实际OHOS库/源码harness手机和PC各core3/session4通过。默认未切换，不代表完整codec/普通HAP/live。
- [x] Task 4A2.7A `6ad44a5`/`15b006c`八格式原生row实现与ABI修复独立审查通过；`2770093` portable Surface长度门独立审查通过。冻结OHOS K/PDU实际库及source程序手机/PC各10项通过，宿主row基准含慢项，目标性能/完整image接线仍待完成，见[像素与长度门验证](../../validation/rdp-pixel-and-surface-gates-20261014.md)。
- [x] Task 4A2.7B `848258c`/`de6b2bb`实际完成计数、stride/crop/padding/八格式image与Bitmap/RDP6/RFX/Set/Stream显式native接线和可选分配门经独立复审通过；host默认None保持，root冻结实际OHOS库/源码消费者手机PC各23+1项通过。目标性能、exact decoded coverage、普通HAP/live及生产选择待完成，见[消费者验证](../../validation/rdp-image-consumers-20261014.md)。
- [x] Task 4A2.8A `6543e07` portable packed真实written/consumed与RDP6完成shape观察接口经独立审查Approved，旧入口原版对照保持；冻结实际OHOS库及source harness手机/PC各5项通过。此门仅完成事实，精确session/baseline覆盖接线与真实首帧仍待完成，见[完成计数验证](../../validation/rdp-completion-facts-20261014.md)。
- [x] Task 4A2.8B `13f1bcc` rich session精确内容/输出索引、完整SW/SH裁剪、RFX实际交集与旧入口禁用collector经独立审查Approved；宿主37/default52/optional两profile和9故障变体通过，冻结实际OHOS source harness手机/PC各37项通过。Adapter完整首帧/恢复接线仍待完成，见[覆盖验证](../../validation/rdp-decoded-coverage-20261014.md)。
- [x] Task 4A2.8C `fa825f1` adapter/baseline精确内容、实际图像借用/整批验证、完整知识恢复及EGFX闭合发布单元独立审查Approved；固定源码OHOS test ELF手机/PC各20+17项通过。其它架构/普通HAP/真实首帧与生产选择待完成，见[首帧覆盖验证](../../validation/rdp-baseline-coverage-20261014.md)。
- [x] Task 4A2.9 `2015400` Compatibility默认与显式RequiredNative策略/实际factory-session-Stage及processor replacement接线独立审查Approved；固定OHOS ELF手机/PC各6项策略和20项编码覆盖通过。完整管线性能、其它架构、普通HAP及生产资格待完成，见[策略验证](../../validation/rdp-native-factory-20261014.md)。
- [x] Task 4A2.10A作者宿主实现/独立原版整管线正确性与测量证据完成，最终独立Spec/QualityPASS；38项全链/35项Stage性能未达标保留。手机/PC O/C/N目标与优化、完整生产资格仍OPEN，见[整管线验证](../../validation/rdp-legacy-pipeline-20261014.md)。
- [x] Task 4A2.10A 手机/PC目标正确性子步骤：冻结618168c OHOS ARM64 release两实例分别实际四项精确测试通过，完整74组两backend/回归判别/持续指针/Stage指针参考，root分别复核原始日志与宿主/设备终态。目标测量与完整资格仍OPEN，见[鸿蒙目标记录](../../validation/rdp-legacy-pipeline-ohos-20261014.md)。
- [x] Task 4A2.10A 手机Stage测量过程子步骤：同冻结618168c的初始与window1顺序补充均实际结束，root核对174组O/C/N及原始终态；18项性能比值未达标和短测量限制保留。仅手机Stage过程完成，PC、全链性能、优化与生产资格仍OPEN，见[鸿蒙目标记录](../../validation/rdp-legacy-pipeline-ohos-20261014.md)。
- [x] Task 4A2.10A PC Stage初始过程子步骤：同冻结618168c的六轮/12任务实际结束，root复核124产物身份及174组原始计数/校验和/终态；保留20项比值未达标、128组原版不足100ms。后续window1原版先行/N-C一轮58组补充也实际结束，root复核149产物及window0/2保持，最终21项比值未达标、129组原版不足100ms；完整性能及资格仍OPEN，见[鸿蒙目标记录](../../validation/rdp-legacy-pipeline-ohos-20261014.md)。
- [x] Task 4A2.10F 色彩预检优化源码、宿主聚焦正确性及实际Mac ARM64对象复审通过；固定六次微测量保留全部短测量与不利样本，未认定稳定性能收益。OHOS/SSE2/fullpipeline/包与生产资格仍OPEN，见[预检验证](../../validation/rfx-color-preflight-20261014.md)。
- [x] Task 4A2.10A 手机完整流水线测量过程子步骤：冻结618168c的控制和全链实际进程已结束，root从原始日志核对50workloads/300组/150比值及18控制记录；保留9项比值、14项warm预算未达标及174组不足100ms的限制。仅测量过程完成，PC全链/两端分配观察及生产性能仍OPEN，见[鸿蒙目标记录](../../validation/rdp-legacy-pipeline-ohos-20261014.md)。
- [x] Task 4A2.10B1/B2 宿主子步骤：checked NEON/SSE2 cursor行叶子与真实fast/slow/cache/pending/input/content/replacement接线完成，独立Spec/Quality审查通过；固定Rust1.96行parity/432原版记录/保护页及最终Mac ARM64直接ABI通过，consumer debug8/release9、graphics5、adapter筛选5、来源14项通过。固定微基准保留短测量和多数decode慢项；当前OHOS RDP release库编译通过，但目标运行/ABI/性能/普通HAP/live及规范alpha仍OPEN，见[光标验证](../../validation/rdp-cursor-native-20261015.md)。
- [ ] Task 4A2.10 完成真实编码输入到Surface/mailbox/Startup的整管线原版对照与目标性能；补齐实际cursor像素热路径及按测量定位的瓶颈，解决SSE2 publication资格并保持默认与codec广告独立。
- [ ] Task 4A2.7 完成八像素格式原生row与image/session接线，补Bitmap真实written计数、source stride/crop/padding和Set/Stream几何门，再验证每目标实际运行、性能、包与生产选择。
- [x] Task 4B1 真实事务→原生呈现、统一视口与输入门接线完成（`87c829a`/`bccb321`），两台合成首帧、触点及本地重绘受限验证，跳帧重试修复经独立复审；不代表真实协议连接，见[呈现验证](../../validation/harmony-native-presentation-20261013.md)。
- [x] Task 4C0 手机/PC 正常权限系统能力与三个接口路线完成受限验证；AVC420 像素通过、EOS 前出帧未通过、精确444失败、HEVC 未报告，见[系统验证](../../validation/harmony-system-codec-20261013.md)。
- [x] Task 4C1 中立 NativeFirst 显式策略 `e1fb9fa` 已实现并通过独立审查；默认桌面顺序保持，尚未接入系统 backend。
- [x] Task 4C2 官方基础镜像静态审计找到双线程缓冲路径及安装 NEON 分派；与三轮运行结果形成高置信度原因解释，未证明当前进程加载身份。当前系统软解不满足静态单帧要求，优先推进合格 FFmpeg 后备，系统优先策略保留。
- [x] Task 4C3A 独立官方 OHOS ARM64 FFmpeg builder/verifier 与专属静态输出线程策略完成（`9c2cd42`/`8343bd3`）；22 项产物/输入/实际链接回归通过，独立复审通过。仅构建证据，普通 HAP loader、逐像素与运行时性能仍待完成，见[后备包证据](../../validation/harmony-ffmpeg-bundle-20261013.md)。
- [x] Task 4C3B 应用自身模块锚定加载器完成（`4f5089e`），47 项宿主测试、OHOS ARM64 构建与公开 NDK 链接通过，独立审查通过；普通 HAP 安装路径/权限与实际加载仍待验证。
- [x] Task 4C3D 普通 HAP 包装与签前/签后字节校验完成（`22513b2`/`dc9419b`），38 项范围测试及独立复审通过；同一最终签名 Debug 包已在手机/PC 安装启动。应用进程内加载和解码仍待 Task 4C3E，包中存在库不代表 DecoderReady。
- [x] Task 4C3E 正常应用内真实 FFmpeg 解码夹具完成（`6a2c91c`），手机/PC 同一签名包各 13/13；无 EOS 首/末帧、独立像素、1440p/4K 合成帧与生命周期通过，OHOS 目录句柄修正保留检查，38 项 FFI/49 项 loader 测试及独立审查通过。未注册生产 codec，运行时 SIMD、持续吞吐与真实远程会话仍待验证。
- [x] Task 4C4 原三库优化应用测量及分格式运行审计完成（`f70b381`），两台基准各 10/10、AVC444/HEVC444 指令级正向证据，独立代码审查通过；夹具再生成脚本小项由 `c9f5217` 修正并通过临时目录完整比较。实际仍存在计算标量分支，不代表生产资格。
- [x] Task 4C5A 单个 AArch64 8-bit H264 luma DC 内核完成（`93e0a6b`），原 C/数值/内存对照 5,974,518 组、30 种 dispatch、checkasm 30 项及本机有界微基准通过，独立审查无问题；后续 `894d5cf` 固定配方与 OHOS 包通过审查，手机/PC各5974518数值用例和新包AVC444真实向量PC、像素/无EOS出帧、10/10基准门禁通过；桌面包与完整codec资格仍待完成，见[内核验证](../../validation/h264-luma-dc-neon-20261013.md)。
- [x] Task 4C5C1 `57d3b4a`/`f6bdda0`显式macOS ARM64来源配方与bundle/app/源码材料整链接通且复审通过；root冻结实际三库六项fixture、同shipping对象5974518数值/30分派、固定开发签名.app门与三个有界基准通过。默认与广告未切，Windows/Linux、HEVC余项及完整资格未完成，见[Mac配方验证](../../validation/macos-h264-recipe-20261014.md)。
- [x] Task 4C5C2 `af28442` Linux来源配方/真实ELF映射/剥离记录/bundle及完整包材料接线经独立审查Approved；12来源及27包合同、3Mac共享接口回归通过。随后冻结37ca646原生CI实际三库/bundle及12插件、6解码fixture通过；初次后置qualification误拒正常SONAME链接；`b93d5e9`别名修复独立Approved，真实run38461630760路径门通过后因GCC配置夹具误用Clang编译失败，`6d6cce0`固定GCC证据修复经独立审查Approved后，冻结6e075c4原生run38467007932通过5,974,518数值/30分派/ABI/保护页、三轮有界基准与完整修改包/目标进程装载；root对应artifact hash及43项输入复核通过，见[Linux配方记录](../../validation/linux-h264-recipe-20261014.md)。
- [x] Task 4C5C3A `9e773ce`/`7c50370`/`42d3403` Windows ARM64固定source endpoint、COFF/map/PE及静态loader写范围接口完成；两轮独立复审关闭全部P2，初始30/修复41/第二轮14项各自范围测试通过。仅本机交叉容器与接口，真实LLVM-MinGW自然构建/strip/package、Windows私有ABI与原生数值/基准待完成，见[Windows接口验证](../../validation/windows-h264-kernel-identity-20261014.md)。
- [x] Task 4C5C3B1 私有数值测试的保护页/时钟最小平台接口完成并经独立源码/执行复审通过；macOS ARM64 contract GREEN、两项精确弱保护 RED、同shipping对象5974518/30分派/ABI/guard通过，原数学与配方保持。Windows adapter/QPC/SEH/native及Linux/OHOS本轮运行待完成，见[私有平台接口验证](../../validation/ffmpeg-private-platform-20261014.md)。
- [x] Task 4C5C3B2A 源码与宿主层：Windows ARM64 私有保护页/QPC及纯整数换算完成，独立源码和宿主执行复审通过；22锚点、10036完整输入、11拒绝负例、10注入逻辑案例通过。首记录脚本失败和仅补两项的续轮均保留。仅本子步骤完成，真实Windows编译/ABI/SEH/native/共享parity/性能/修改包仍待完成，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2B 源码准备子步骤：独立Windows ARM64 ABI/SEH helper、外层恢复与逐寄存器assembly负例完成并经静态Spec/Quality审查通过；仅源码提案，无编译或原生SEH结果。依赖Cdriver、固定工具链COFF/PE、真实Rtl*回溯/异常/完整输出oracle和全部生产资格仍待完成，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2C 固定LLVM-MinGW实际COFF子步骤：冻结122f44e/run38516856053的19项窄检查、三个正确对象/bad1–4实际编译、bad5精确MC拒绝、实际工具/源/对象/元数据均复审通过。仅compiled-only；Windows原生Cdriver/SEH/parity/基准/包仍OPEN，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2E 固定C/PE编译接口源码准备：新runner/25项窄故障控制/专项workflow及两轮证据终态修复均完成独立源码复审。首次真实run38531352368的19+25checks通过、variant0 C/link/readobj成功后，被错误NTDLL导入假设拒绝，失败材料保留。导入修复的29项及独立源码复审通过；后续run38542776507的variant0通过compiled-only，variant1因无用VirtualUnwind被优化删除而误拒。第四轮变体要求修复与36项控制源码复审通过；2026-10-15核对第三run38548996801，19+36检查/七COFF/五C-PE及独立静态产物验收通过，完整内层ZIP及外层loose隐藏workflow遗漏边界已记录；两次失败保留，Windows原生验证仍OPEN，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5D1A HEVC16原版C/独立floor模型119600 cases、完整context、精确原版selector RED及有限Mac保护页前置门完成，独立审查通过；不代表NEON或生产资格，见[原版参考验证](../../validation/hevc-intra16-reference-20261014.md)。
- [x] Task 4C5D1B 源码与Mac私有正确性子步骤：两个8-bit HEVC16 NEON核、实际selector/119600数值/4800共用guard路径/1472最小guard/73600直接ABI、六类compiled负例及PRIVATE禁用header门经独立最终Spec/Quality审查通过；确定性取整负例P2以实际233/233/241配对控制关闭。仅本子步骤完成，性能/其它平台与容器/正式配方包/完整D1B仍OPEN，见[HEVC私有内核验证](../../validation/hevc-intra16-native-20261014.md)。
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
