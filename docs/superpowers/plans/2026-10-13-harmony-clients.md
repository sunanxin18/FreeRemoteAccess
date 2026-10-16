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
- [x] Task 4A2.10B3 OHOS ARM64 光标执行子步骤：测试专用共享 Mach-O/ELF 重定位与实际最终程序经独立 Spec/Quality 审查通过；固定 K/RDP release ELF 在手机、PC 分别通过默认忽略预检、精确直接 ABI（含 x25/x29 负控制）和三个真实 factory/Stage/content 消费者测试，每项均有独有 guest/host exit0。目标性能、规范 alpha、普通 HAP 和真实远程连接仍未完成，见[光标验证](../../validation/rdp-cursor-native-20261015.md)。
- [ ] Task 4A2.10 完成真实编码输入到Surface/mailbox/Startup的整管线原版对照与目标性能；补齐实际cursor像素热路径及按测量定位的瓶颈，解决SSE2 publication资格并保持默认与codec广告独立。
- [ ] Task 4A2.10C SSE2 publication：2026-10-16当前源码预检确认RequiredNative只需SSE2而RGBA→BGRX仅有SSSE3/标量，SSE2-only存在实体缺口；两文件共享SSE2向量主体/尾部及真实ISA否决测试已实现；固定Rust1.96 ARM64 debug/release各8项通过、四非Mac x86目标实际优化对象编译通过。Windows/Linux i686局部CI接线经独立配置审查通过；ba603575普通CI的Linux/Windows x86_64模块各12项、dd46ea84的Linux i686与263d7bf的Windows i686各11项实际GREEN（均含三个ISA测试），旧体RED及最终ISA/ABI/guard/性能或生产资格尚未完成。Linux专用non-test parity/保护页程序及有界child清理经源码复审；59cdd705两目标普通release ELF各完成每kernel10240parity/160合法guard及五精确故障负控，独立运行复核通过。固定旧体反例及严格接线经源码审查，cd4dbc8的Linux x64/i686均实际compile0/run101且唯一counter左0右1，原件及独立读数通过。Windows私有ordinary入口/owner及现x64/i686矩阵固定接线经独立源审，实际guard/ABI/perf仍待验。SSSE3优先、ARM64与默认策略保持，见[转换验证](../../validation/rdp-sse2-publication-20261016.md)。
- [x] Task 4A2.10 coverage包装器分配注入修复：default/optional debug/release实际源码四组48/49/63/64通过，真实receipt/cursor失败注入及八个可选大分配保护子进程保留，452生产源码不变；独立复审/交付结果见[包装器验证](../../validation/rdp-decoded-coverage-allocator-20261015.md)。此项不关闭整管线性能、其他包装器或目标执行。
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
- [x] Task 4C5C3A2 Windows显式配方源码/宿主子步骤：2026-10-16仅明确ARM64修改配方启用独立fresh attempt、自然工具原调用与失败partial保留、Windows对应源码exact材料和现核接口适配。map共享覆盖及真实configure append/eval转义两轮修复经独立复审6499ea16关闭；初始/后继受影响host控制及实际旧RED保留，pwsh/Pester环境缺失不计通过。默认upstream三架构与其他平台不变，仅source/host资格。
- [ ] Task 4C5C3A3 Windows固定工具链真实自然构建：单独显式CI authority、原始双通道/工具字节及全量POSIX运输接线已实现；初审两P2经Fix1独立复审关闭，root最终运输控制22/22通过。真实Windows/WSL控制仍待执行，首次受限注册run38847486035在initialize阶段失败且未到达FFmpeg，原错被formatter插值遮蔽、无artifact；同workflow后继诊断复现多候选路径拼接，修后run38850194932六项bootstrap实际控制全部通过；自然入口恢复经审查后run38851041806在checkout实际exit1，本机真实helper复现初始化已建目录与checkout再次mkdir冲突；最小修复/26项宿主控制及run38853042725实际Windows前缀通过，16源与六工具原件已核对，后继59cdd705已进入WSL依赖安装，但SID/PGID后代归属拒绝且cleanup未证明停止，具体身份待查、未到FFmpeg；四源固定APT诊断经独立审查后，bd4bb24实际两图/同UUID定位组外dpkg分支，停止仍未证明，Use-Pty最小修复及原单项RED→GREEN源审通过，待fresh实际验证，不归零旧run，尚未取得完整自然构建结果。原自然AS grammar与安装复制后strip的现接口拒绝保持，须按实际首错另审；后继B同对象数值组合、C原生ABI/guard/qmul/QPC及三个fresh-process基准、D修改包安装/实际装载全部必需，不由C0或source/host通过替代。
- [x] Task 4C5C3B1 私有数值测试的保护页/时钟最小平台接口完成并经独立源码/执行复审通过；macOS ARM64 contract GREEN、两项精确弱保护 RED、同shipping对象5974518/30分派/ABI/guard通过，原数学与配方保持。Windows adapter/QPC/SEH/native及Linux/OHOS本轮运行待完成，见[私有平台接口验证](../../validation/ffmpeg-private-platform-20261014.md)。
- [x] Task 4C5C3B2A 源码与宿主层：Windows ARM64 私有保护页/QPC及纯整数换算完成，独立源码和宿主执行复审通过；22锚点、10036完整输入、11拒绝负例、10注入逻辑案例通过。首记录脚本失败和仅补两项的续轮均保留。仅本子步骤完成，真实Windows编译/ABI/SEH/native/共享parity/性能/修改包仍待完成，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2B 源码准备子步骤：独立Windows ARM64 ABI/SEH helper、外层恢复与逐寄存器assembly负例完成并经静态Spec/Quality审查通过；仅源码提案，无编译或原生SEH结果。依赖Cdriver、固定工具链COFF/PE、真实Rtl*回溯/异常/完整输出oracle和全部生产资格仍待完成，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2C 固定LLVM-MinGW实际COFF子步骤：冻结122f44e/run38516856053的19项窄检查、三个正确对象/bad1–4实际编译、bad5精确MC拒绝、实际工具/源/对象/元数据均复审通过。仅compiled-only；Windows原生Cdriver/SEH/parity/基准/包仍OPEN，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2E 固定C/PE编译接口源码准备：新runner/25项窄故障控制/专项workflow及两轮证据终态修复均完成独立源码复审。首次真实run38531352368的19+25checks通过、variant0 C/link/readobj成功后，被错误NTDLL导入假设拒绝，失败材料保留。导入修复的29项及独立源码复审通过；后续run38542776507的variant0通过compiled-only，variant1因无用VirtualUnwind被优化删除而误拒。第四轮变体要求修复与36项控制源码复审通过；2026-10-15核对第三run38548996801，19+36检查/七COFF/五C-PE及独立静态产物验收通过，完整内层ZIP及外层loose隐藏workflow遗漏边界已记录；两次失败保留，Windows原生验证仍OPEN，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2F1 严格日志解析器源码/宿主控制：2026-10-16纯bytes七固定组合、独立60masks/8PC差值及六child异常/continue/独立wait终态链完成；独立审查两项Important（BOOL非零/最后退出线程）经Fix1和同reviewer复审关闭，22项synthetic host-only与语法通过。无PE执行/runner/CI/native资格，详见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2F2 静态产物验收模块：2026-10-16精确七文件transport、独立来源/context、三个innerZIP及源码/七COFF/五PE实际字节闭合完成；独立审查I1缓存代码绕过经compiler/support两条路径直接执行已校验源码bytes修复，两个独立控制先RED后GREEN并复审通过。可移植21pass/1可选skip，旧compiled-only材料显式合成transport静态22pass；无PE执行，新workflow/原生runner尚未接线，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2F3 源码与同run CI接线子步骤：2026-10-16完成Win32实时machine门、创建时关联Job、固定七命令、原始bytes与wait/jobzero/有界清理、认证API原始ZIP下载和最终材料封装；精确七文件upload与Windows ARM64依赖job已接线。独立审查三项Important经Fix1复审关闭；首CI后Fix2增加checkout显式LF和pin拒绝前身份材料，45pass/1可选skip并经同reviewer批准。仅源码/宿主层完成，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2F3 有限原生运行验收：2026-10-16 `a4e35c7` 原run38827578424/attempt1固定工具链及同run五PE、七命令真实wait0/exit0/jobzero通过；root与独立执行复审230ce140核对678outer/675inner、624 sealed files、原8PC/新12fragment PC、v0/v4回溯及六子严格异常/终态，预算不变。仅关闭固定C0 nativehelper-control，完整CONTEXT仍C内部oracle；generic/shipping/客户端资格继续OPEN。原先各失败全部保留，见[Windows私有平台验证](../../validation/windows-private-platform-20261014.md)。
- [x] Task 4C5C3B2F3 失败专用诊断source/host：线程context、pre-call基线、实际module/file/hash/code和失败parser已冻结；初版128控制127通过/1旧工件可选skip，独立审查I1读取依赖缺口经真实RED10反例、24项parser回归及定向复审关闭。只增加最终失败锁存，原异常/ASM/Continue/预算/exit/wait保持；新00b328c原run38812955596编译成功，完整失败包与同run静态输入已核对，register0/unwind0成功但isolated额外AV失败；实际FP=0与ntdll故障LDR/x9=0x90已取得，H/O位置后经独立审查，完整native/保存内容/dataflow仍OPEN，见同一Windows私有平台验证。
- [x] Task 4C5C3B2F3 失败路径展开架构预检：2026-10-16 逐回调 SP/FP 权威、私有固定代码契约 C0、调用前 selector、32B SP phantom fragment 与不改指令的16B wrong_return inherited-H metadata 方案经独立 Spec PASS / Quality Approved。符号模型/一次独立 Clang15 COFF 只支持架构提案；没有新固定 LLVM20/PE/native 证明。原失败、完整七命令、通用 checker 与 shipping 资格仍 OPEN，见同一Windows私有平台验证。
- [x] Task 4C5C3B2F3 失败路径协调修复源码/宿主：C0 selector、独立片段 metadata、固定 child 路由与精确两重定位/owner 认证、12 PC 完整 CONTEXT 副本 oracle、F1/F2/pin 链已实现并经独立 Spec PASS / Quality Approved。158 项宿主控制中157通过、1既有可选旧工件跳过；实际Clang15 COFF/36上下文独立模型及7负控通过，generic/旧negative指令保持。无新的固定LLVM20 C/PE或系统Rtl执行；原native失败及下面原生验收门继续OPEN。
- [x] Task 4C5C3B2F3 精确诊断清单修复：7406原run38824097940编译成功但F2漏接固定六函数120诊断而七项未尝试；四文件固定清单/独立回归与pin传播修复经Spec PASS/Quality Approved。53项受影响host通过/1旧可选skip，原同input完整静态接收五PE及seal/真实内容负控通过；原run不改为成功，新原生七命令仍OPEN，见同一Windows私有平台验证。
- [x] Task 4C5C3B2F3 失败路径修复与有限新原生验收：C0 selector/两重定位/片段owner/trap与+4/完整CONTEXT副本oracle和F1/F2/pin经源码审查；a4e35c7同run原七命令及独立执行审查通过。原失败保留，不以私有C0成功替代generic恢复、真实kernel/数值/guard/性能/包或客户端远程登录。
- [x] Task 4C5D1A HEVC16原版C/独立floor模型119600 cases、完整context、精确原版selector RED及有限Mac保护页前置门完成，独立审查通过；不代表NEON或生产资格，见[原版参考验证](../../validation/hevc-intra16-reference-20261014.md)。
- [x] Task 4C5D1B 源码与Mac私有正确性子步骤：两个8-bit HEVC16 NEON核、实际selector/119600数值/4800共用guard路径/1472最小guard/73600直接ABI、六类compiled负例及PRIVATE禁用header门经独立最终Spec/Quality审查通过；确定性取整负例P2以实际233/233/241配对控制关闭。仅本子步骤完成，性能/其它平台与容器/正式配方包/完整D1B仍OPEN，见[HEVC私有内核验证](../../validation/hevc-intra16-native-20261014.md)。
- [ ] Task 4C5 优先补 AArch64 8-bit H264 luma DC 变换/反量化；维护显式补丁来源、C oracle、逐核 parity、目标基准和普通 HAP 同流验证。HEVC 16×16 planar/angular 与已识别的 32×32 angular 标量分支随后处理，避免重写已向量化分支。
- [ ] 优先完成 AVCodec Kit 逐设备能力与精确 fixture 验证、系统硬解/软解 adapter 和生命周期接线；软解保留 SIMD 证据要求。系统后端未覆盖的格式再使用经过 OHOS ABI/parity 验证的 FFmpeg 后备。
- [ ] 确认 ARM64 Bitmap/RemoteFX/像素转换及生产 dispatch 满足现有 SIMD gate；独立叶子不能替代完整解码路径。
- [x] Task 4D1 普通应用 RDP 组合源码/FFI子步骤：2026-10-15 显式默认关闭 `normal-hap-rdp-validation` 沿用同一 C ABI/AppEngine/SessionHost，OHOS ARM64 仅注入 Unix/RequiredNative/LegacyOnly factory；宿主回归、实际最终 FFI 链接与独立 Spec/Quality 审查通过，见[组合验证](../../validation/harmony-rdp-composition-20261015.md)。默认与生产广告没有提升。
- [x] Task 4D2 普通 HAP/安装/表单子步骤：实际 strip 绑定失败经插件证据定位并修正、复审和真实重放通过；开发签名/签后身份检查及两台专用模拟器安装、普通表单、Windows/Mac目标选择和实际截图检查通过。仅本子步骤完成，真实认证、首帧、刷新、输入/IME、生命周期与重连仍未完成，见[包与运行验证](../../validation/harmony-rdp-normal-hap-20261015.md)。
- [x] 手机普通HAP真实会话子步骤：2026-10-15 授权Windows恢复后真实桌面、保存/读取凭据后重连、数分钟刷新、横屏触控开始菜单和主动断开已观察；同进程主动断开后重连及新进程HUKS读取后登录、竖屏触控也已观察；首次短会话退出根因、键盘/IME、完整生命周期及PC独立验收未完成，见[真实会话记录](../../validation/harmony-rdp-live-20261015.md)。
- [ ] 模拟器连接授权 Windows：保存/读取凭据、首帧、增量、触控/键鼠、远程软键盘入口与 IME 提交、断开/重连与恢复。
- [ ] AVC/HEVC广告继续受原精确门禁控制；未通过的 codec 不由新平台构建自动开放。

### Task 5: Mac 实际连接与平台媒体

**Files:** OHOS composition root、native media backend、包验证脚本。

**Interfaces:** 注册现有 Apple factory；平台 codec 注入既有 `VideoDecoderRegistry`，不修改Apple wire或在Mac安装程序。

- [x] Task 5A1 MVS IDCT 源码/宿主/最终ELF子步骤：2026-10-15 默认关闭的 ARM64 i64 NEON 变换，原标量 oracle 保持，28,800块差分和完整Apple feature512项回归通过；初版实际代码生成48条标量乘法的失败保留并修正，最终指令和源码独立Spec/Quality审查通过。目标执行见下一行；颜色、完整MVS、性能、应用组合及真实Mac会话仍OPEN，见[IDCT验证](../../validation/mvs-idct-arm64-20261015.md)。
- [x] Task 5A1 OHOS目标执行子步骤：同一已审查最终ELF及官方SDK C++库在手机/PC各完成9项IDCT聚焦+6项full/type-1精确消费者，14条命令共30次通过且host/独有guest退出均0；两端28,800块差分及前后产物身份经root核对。权限观察与脚本空格误拒的先前失败保留，不代表性能、完整MVS、HAP或真实Mac会话完成，见[IDCT验证](../../validation/mvs-idct-arm64-20261015.md)。
- [x] Task 5A2 色彩/像素源码/宿主/最终ELF子步骤：2026-10-15 默认关闭的ARM64小端NEON色彩和RGB/BGRX/u32路径接入full/type-1/surface；原标量oracle与默认分配策略保持，完整色域与mode-4全域、default/colors各519及aggregate522项回归通过，最终ELF/四个叶函数独立Spec/Quality通过。目标执行见下一行；完整整管线差分、性能、HAP/应用及真实Mac仍OPEN，见[色彩/像素验证](../../validation/mvs-pixels-arm64-20261015.md)。
- [x] Task 5A2 OHOS目标执行子步骤：同一最终aggregate ELF在手机/PC各完成14项像素+3项精确消费者，8条命令共34次通过执行且实际host/独有guest退出均0；两端完整色域/mode-4与前后产物身份经root独立复核。CLI范围之外的完整MVS、性能、HAP/应用和真实会话仍OPEN，见[色彩/像素验证](../../validation/mvs-pixels-arm64-20261015.md)。
- [x] Task 5A3 指定完整记录/状态/发布宿主差分子步骤：2026-10-15 三个测试专用序列的scalar/native38条70,802字节全值相等，cache/Cr19Cb14/代际owner/回滚和补丁覆盖；截断负例提前geometry拒绝的P2已用真实RED和精确parse链修正并复审通过，两份fix1 OHOS最终ELF链接成功。目标执行见下一行，其它计算热路径/性能、普通应用与真实Mac仍待完成，见[流水线验证](../../validation/mvs-pipeline-arm64-20261015.md)。
- [x] Task 5A3 手机/PC固定产物执行子步骤：两端scalar/native各3项，4条命令共12次通过且实际host/独有guest退出均0；四份38条70,802字节与冻结宿主全部值及顺序精确相等，前后源/产物身份经root独立核对。仍不代表性能、全部MVS、HAP或真实Mac，见[流水线验证](../../validation/mvs-pipeline-arm64-20261015.md)。
- [x] Task 5A4 输出映射/位图行源码与最终ELF子步骤：2026-10-15 既有default-off后端补全NEON环绕范围映射及mode3/4/solid行；保留原oracle/数学/wire/cache/代际契约，独立Spec/Quality通过，default/aggregate完整包527/531及新聚焦5/6通过，官方OHOS最终ELF/实际指令通过；见[原生补全验证](../../validation/mvs-native-completion-arm64-20261015.md)。
- [x] Task 5A4 手机/PC固定产物正确性子步骤：同一最终ELF两端各6项聚焦+3项pipeline，共4条实际命令/18次通过且host/独有guest均0；38条/70,802字节完整值与两份宿主及两端逐字相等，root独立核对43源/79运行证据及前后产物身份。raw copy资格、性能、普通HAP Apple和真实Mac仍OPEN，见[原生补全验证](../../validation/mvs-native-completion-arm64-20261015.md)。
- [x] Task 5A5 type-0 RGB copy源码/宿主/实际ELF：两个helper接入精确ARM64 3..24字节copy叶，保留forward别名/混合来源、默认原循环与type-1完整快照；独立Spec/Quality通过，default/aggregate focused各11、完整包538/542及pipeline全值连续性通过，见[精确拷贝验证](../../validation/mvs-copy-arm64-20261015.md)。
- [x] Task 5A5 手机/PC固定产物正确性：同一ELF两端各11项copy+3项pipeline，共4命令/28次通过，44源/输入前后身份、24实际HDC/20唯一guest退出及38条70,802字节全值顺序经root独立核对。性能、普通HAP Apple和真实Mac仍OPEN，见[精确拷贝验证](../../validation/mvs-copy-arm64-20261015.md)。
- [ ] Task 5A6 有限MVS整管线性能：8真实wire工作集、宿主default/native完整32记录+8state差分和两个官方OHOS ELF及独立审查完成；宿主和手机分别完成固定ABBA实际384条measured，copy有收益、dense-Y/refine原生较慢，保留共同libtest/后台负载限制。手机2026-10-16结果只归属fb78冻结候选，当前源码漂移已记录；入口只准确计apply组合/publish/total。手机和PC分别完成两ELF各32条资格/三组完整值对比，PC正式计时、当前HAP及生产性能仍OPEN，不由此宣称帧率，见[有限管线记录](../../validation/mvs-pipeline-measurement-20261015.md)。
- [x] Task 5A6 手机有限计时子步骤：原fb78候选四个固定进程实际guest/host0、384 measured/128 warmup及完整记录/状态比较通过；720p copy中位比0.714，dense-Y/refine1.177/1.223。前后ELF/SDK/reference/current-input摘要保持，三次执行前辅助门停止保留，PC未观察/操作，详见同一验证记录。
- [ ] Task 5A7 IDCT性能晋升：CSD源码正确性及固定手机对照完成，但结果MIXED，性能晋升未通过、仍OPEN。保留默认关闭实验，不改变生产选择，不追加既有候选计时直到变好；后续独立DC候选不改写原MIXED结论；PC、HAP和真实Mac未由此证明，见[CSD验证](../../validation/mvs-idct-csd-arm64-20261016.md)。
- [x] Task 5A7 CSD源码/宿主/实际ELF：冻结同一当前源码N0后仅改i64双lane常量helper，直接i128 oracle、全域界、28,800块差分及完整555项回归通过；OHOS最终入口665→542条静态指令，独立Spec/Quality通过，不将静态减少称实测加速。
- [x] Task 5A7 手机有限资格/计时：新N0/N1各32record/8state完整输出与宿主及彼此逐字相等；固定N0/N1/N1/N0共384 measured/128 warmup，实际host/guest0。dense-Y中位比0.940314，refine合并1.012807，copy/cache miss等更慢，保留全部点及分组结果、共享宿主/共同测试成本限制；执行后报告处置独立APPROVE，仅保留默认关闭实验。PC继续人工填写，未观察/操作。
- [x] Task 5A7 effective-DC源码/宿主/静态ISA子步骤：同一基线先冻结共享测试N0，仅改原生IDCT为精确effective-AC判定及signed i64 DC早返；16777216全域、560项完整宿主、fresh32+8三组完整字节比较及两个最终ELF完整调用闭包通过，独立Spec PASS/Quality Approved。随后单独手机固定CLI共8项直接及两版各32+8资格通过、三组完整41文件逐字相等；手机原固定八工作集ABBA另行释放并完成384measured/128warmup，dense-Y/refine中位比0.730/0.756但overlay/type1-copy为1.063/1.163，保留全部原始点/aggregate/分组及共享宿主限制，结果MIXED。默认OFF，PC未操作，性能晋升仍OPEN，见[DC候选验证](../../validation/mvs-idct-effective-dc-arm64-20261016.md)。
- [x] Task 5B1 标准MVS普通应用实验组合：独立默认关闭OHOS ARM64 feature，可与RDP共存；保持Mac自动HP语义/音视频不可用及既有输入门，真实最终FFI/架构/互斥/依赖及构建参数验证完成、独立Spec/Quality通过；root冻结后36项宿主与7脚本通过。OHOS正向用例只编译、不由宿主mock证明注册，见[组合验证](../../validation/harmony-apple-composition-20261015.md)。
- [ ] Task 5B2 正常表单明确模式选择：源码及双协议普通Release HAP构建完成，完整FFI 42通过/1既有ignored和精确strip包身份已核对。2026-10-16新签名包29b4022b的手机标准/特大字号×浅/深色×横/竖屏8格通过实测及独立执行审查：系统/模式名与完整载入提示可读，普通表单同页、大字四格按钮滚动可达并返回；原Fix1/2/3失败保留。新包初次登录TCP超时；用户确认后单次重试TCP成功、协议surface2760×1256，但约3秒内回登录，原因待诊断；后续仅加终态诊断的新签包fdb597c2单次现有安全profile登录取得真实完整桌面/横屏存续及约218.8秒后主动断开，旧退回未复现且根因仍UNKNOWN。不能关闭持续刷新、长期稳定会话、全部凭据交互、键盘/IME或PC门；PC人工填写继续hold。见[登录外观](../../validation/harmony-login-layout-20261013.md)和[模式与HAP](../../validation/harmony-mode-selection-20261015.md)。
- [ ] Task 5B3 同一组合HAP：2026-10-15开发签名/官方验签/双参数精确checker及原28个条目逐字身份已完成，签名包3fbdd046；手机已安装，随后方向纠正包539cf8ee也已真实Windows登录/旋转验证；PC新包GUI及stockMac真实认证/首帧/刷新/指针/重连待验证，不替代完整键盘/IME、音频、HP与性能资格，见[模式与HAP](../../validation/harmony-mode-selection-20261015.md)。
- [x] Task 5C 共享接口/源码子步骤：physical_key_input与text_input三层独立交集、Apple truthful能力发布、RDP既有输入保持及桌面事件路由完成；真实InputRouter反例修复经独立复审PASS，宿主核心1056 unit/Apple5 integration/Platform doc、desktop171+2通过，23既有ignored保留。OHOS Release包签验通过；真实键盘/IME及各原生平台运行仍待验证，见[输入能力验证](../../validation/input-capabilities-20261015.md)。
- [x] Task 5C1 OHOS有限实体键源码子步骤：101 HID映射/入队前拒绝/真实来源Up清理、仅OHOS ARM64显式backend及text=false经独立Spec/Quality审查通过；69宿主FFI、67 ETS内部用例及实际Release双协议HAP构建通过。Pause/国际键/ISO扩展保持未资格，未签装或操作PC；wire consumer固定oracle、实体键/锁态/布局与真实IME仍待完成，见[有限键盘验证](../../validation/harmony-physical-keyboard-20261015.md)。
- [x] Task 5C1R RDP实际键consumer：7项独立固定oracle覆盖101 HID最终FastPath code/flags及held/ReleaseAll/repeat/stop/锁键/未知键，focused7/完整359通过、11原ignored，生产及原测试不变；独立复审通过。Apple consumer、各目标原生执行及手机/PC真实键盘/IME仍OPEN，见[有限键盘验证](../../validation/harmony-physical-keyboard-20261015.md)。
- [x] Task 5C1A Apple实际键consumer：101 HID独立plain/shift固定oracle经实际PointerWireState与writer worker进入Standard/HP loopback，10项聚焦及完整默认Apple550 unit/5 integration通过、11既有ignored；生产/旧测试逐字节保持，独立Spec/Quality通过。仅宿主字节消费者，不授予Mac/设备/锁态/IME资格，见[有限键盘验证](../../validation/harmony-physical-keyboard-20261015.md)。
- [x] Task 5C2A 有来源IME Owner桥源码子步骤：独立版本化source/operation ABI、有界窗口来源/待发队列、真实focus/代际/呈现门及本地serial状态完成，三项独立审查缺口经确定性RED→GREEN及复审关闭。最终90项FFI（21项IME）/86项app及实际Release双协议HAP构建通过；未签装、普通组合根仍IMEUnavailable，前端系统IME与手机/PC资格未完成，见[来源桥验证](../../validation/harmony-ime-owner-20261015.md)。
- [x] Task 5C2B/3 正常IME前端源码/HAP子步骤：有界UTF16 editor、实际SDK adapter、公开ICU桥、浮动岛键盘与OHOS ARM64组合接线完成；初审真实focus epoch反例经RED→GREEN和独立复审关闭，最终受影响80项ETS与实际双协议HAP通过，未改16个其余冻结源。原standalone ICU固定runner真实失败（资源缺失），后续手机普通HAP/NAPI固定合同实测通过并恢复签验正式包，真实IME/远端文字和PC仍OPEN，见[输入法验证](../../validation/harmony-system-ime-20261015.md)。
- [x] Task 5C2N 手机正常NAPI固定字素合同：隔离片独立审查、签验、PID/时间/专用tag完整终态通过；已恢复签验正式包，原HUKS记录可读取。仅该固定helper门完成，原CLI失败保持。正式包重连显示桌面，但真实IME唤起两次明确未通过；继续定位阶段，PC保持不操作，见[输入法验证](../../validation/harmony-system-ime-20261015.md)。
- [x] Task 5C2R RDP文字wire/writer回归：真实command→drain→固定encoder→writer三组独立字节oracle，长串/非BMP跨包/FIFO及空stop完成，3新增/362完整通过、11既有ignored，独立Spec/Quality通过；生产与旧测试字节不变，仅宿主层，不授予IME/live资格，见[输入法验证](../../validation/harmony-system-ime-20261015.md)。
- [x] Task 5C2P1 Ability几何生命周期与诊断源码/HAP子步骤：实际Ability→Index→adapter反例修正候选、121项宿主/最后46项受影响复跑、最终官方双协议HAP与checker完成；初审诊断归属I1经Fix1六交错及32+13+7回归转绿、正式HAP及独立复审通过，仅本源码/HAP子步骤完成，尚未授予真实IME资格，原手机generic根因仍待阶段实测，见[输入法验证](../../validation/harmony-system-ime-20261015.md)。
- [x] Task 5C2P2 Native XComponent焦点源码/HAP与手机焦点门子步骤：P1新包手机真实日志stage2/outcome6、gates895缺前端focused，尚未调用SDK；libraryname配置下ArkTS焦点回调不响应，绑定实际surface/代际/focus epoch的原生token候选已完成67项ETS、实际生产C++/NAPI host consumer及官方BOTH/checker，Fix1以worker串行查询及显式返回意图完成94项FFI/23项surface/7项生命周期与正式BOTH，复审关闭I1/I2，但新增I3旧意图可跨native generation；Fix2原生身份绑定已冻结，30项surface/7项生命周期、真实NAPI metadata→Index及正式BOTH/checker通过，限定独立复审Spec PASS/Quality APPROVE、I3关闭，已签验安装手机并真实越过焦点门，下一失败为首项insertText订阅12800009、SDK attach尚未执行；保持真正失焦/窗口/会话屏障，不能伪造focus=true。实际手机SDK/输入法及其它事件的接线和真实路由仍须分别确认与验收，见[输入法验证](../../validation/harmony-system-ime-20261015.md)。
- [x] Task 5C2P3 SDK绑定/订阅源码/HAP与手机有界实测子步骤：依据手机新进程阶段日志与公开API，先完成attach再订阅、在完整来源确认后显示键盘，保持异步SDK串行与失效清理；107项受影响回归、正式BOTH/checker及独立Spec/Quality通过，冻结4源仅modules.abc变化；签验新包在API26手机真实显示系统键盘/中文候选，重复中文提交、删除、换行和英文字母到达Windows记事本，测试后不保存并断开；PC/真机/实体键盘/非BMP实测及长时仍OPEN，见[输入法验证](../../validation/harmony-system-ime-20261015.md)。
- [ ] Task 5C 中立物理键/提交文本能力分离与真实输入：修正Apple Text宣称与实际不处理的不一致，保留既有桌面/RDP行为，OHOS未满足门的输入明确不可用。
- [x] Task 5D共享接口/OHOS接线及手机有界尺寸证据：真实physical/workarea/content/scale经独立消息与Owner FIFO进入NativeDisplay；手机真实竖屏1256×2760以Landscape投影2760×1256，实际RDP协议surface及完整桌面同尺寸。Fixed/ServerManaged/其他模式与动态门保持；初审ArkTS/工作区边界问题修复后独立复审及最终Release签验通过，见[显示几何验证](../../validation/harmony-display-geometry-20261015.md)。本项仅关闭该实现与手机观察子步骤。
- [ ] Task 5D剩余验收：PC实际包/会话/搬窗/外接屏、协议请求与服务端ACK直接证据、真机与完整多显示器场景仍需分别验证，不由手机投影/surface匹配视作完成。
- [ ] OHOS 系统 H.264/HEVC 和音频能力优先按既有 media 契约验证精确格式/生命周期；AAC MIME 不代表 AAC-ELD。未覆盖路径保留 fdk-aac/FFmpeg 后备并完成真实交叉构建及目标执行。
- [ ] 授权Mac完成真实认证、首帧、持续更新、输入、重连；音频/剪贴板只报告已实现并验证的能力。
- [ ] 跑Windows/macOS/Linux相关回归及包检查，记录本轮没有真机的平台。

### Task 6: 鸿蒙手机系统原生服务证据与交付矩阵

**Files:** 新增 `docs/validation/harmony-native-service-20261013.md`；更新 `README.md` 客户端/服务端两轴矩阵。

- [x] 核对官方API、系统原生服务入口及模拟器实际可用服务；不采集其他应用秘密、不使用调试注入实现产品功能。
- [x] 有可验证协议才新增独立 adapter，先确定性fixture再真实互操作；无服务/权限证据则明确具体缺口，不造假可用目标。
- [ ] 记录每个HAP/架构/模拟器/协议的版本和验证范围；保留真实未完成项。
- [ ] 完成独立最终审查、格式、共享测试和受影响平台构建；commit/push按用户交付授权执行。


### 手机登录成功后横屏（2026-10-15 用户纠正）

- [x] 手机登录前USER_ROTATION_PORTRAIT，真实page3+session成功后USER_ROTATION_LANDSCAPE一次，随后自动旋转；回登录竖屏，请求串行/窗口代际去重，PC和manifest保持。旧创建即横屏策略已取代。
- [x] 新策略独立审查/11测试、普通双协议Release HAP构建签装、手机真实Windows登录后自动横屏、随后竖屏保持与断开回竖、五位端口完整显示已受限验证；历史包与当前包/PC边界分开记录，见[方向验证](../../validation/harmony-phone-orientation-20261015.md)。
- [ ] 手机深色/最大字号复验、系统旋转锁切换、完整键盘/IME与真机方向验收。

### 鸿蒙右下角会话浮动岛（2026-10-15 用户新增要求）

规格见[会话浮动岛](../specs/2026-10-15-harmony-session-island.md)。用户明确选择Harmony局部覆盖，释放额外56vp产品工具条；手机/PC分别验证，其他平台不改。

- [x] 独立shell策略/固定Material glyph、右下局部命中岛、全尺寸XComponent、PC原生chrome；本地/远程输入来源合同及主机回归通过（真实键盘/IME不由此完成）。
- [x] 独立审查、实际普通双协议HAP/签后资源身份检查、两端安装、本地PC窗口与手机Windows会话/浮动岛/触控验证。
- [ ] PC真实会话浮动岛、完整键盘/IME、复杂多点/拖动和真机；等待安全密码框手动输入不阻塞已完成源与包交付。

2026-10-15 浮动岛最终源与包：右下局部Stack释放56vp工具条，PC原生chrome恢复。独立Spec/Quality通过，23岛策略+11方向、root完整FFI51通过/1既有ignored，实际Release BOTH构建签验/两端覆盖安装完成（signed1bdf2007、packagedFFI2b4cad31）。手机真实Windows/HUKS、深色、成功后横屏、相同2652×1158画面矩形、自动收起/详情保持/关闭后与隐藏后首触控/断开通过；PC最终正常窗口已观察，真实登录仍等安全框手动输入。native尾Up原来源跨控制/几何屏障的源合同修复不等于真实键盘/IME验收；菜单局部刷新与比例黑边保留为独立graphics/resolution问题。详见[最终浮动岛验证](../../validation/harmony-session-island-20261015.md)。全路线图仍未完成。
