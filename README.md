# FreeRemoteDesk

2026-09-08 当前进度：macOS 仅支持 ARM64。Linux 应用入口整合后的完整工作区测试 1705 passed / 0 failed / 16 ignored；此前 `85324f5` ARM64 release 包验证通过。默认生产 RDP 仍为 LegacyOnly；显式 `--rdp-egfx-experiment avc420|avc444` 用于采集互操作证据。

`707b178` 的1280×720探针取得20秒/70帧 ClearCodec/Progressive混合流，实际AVC计数0。该实验GUI的原生分辨率cache发布失败已由 `64dd9e1` 修复：2560×1440显示器原生模式下真实桌面、开始菜单鼠标、搜索键盘输入、正常断开与退出已受限验证；约三分钟会话最后诊断110帧、失败0。仍不能把混合流归为H.264验收。

`5c00935` 的 Windows run `34192818981`、Linux run `34192818977` 与 macOS run `34192818985` 全部成功，包括 Windows ARM64 原生 runtime。七目标分别通过 Progressive/邻近 EGFX 59 项回归及内核基准；该证据不含后续窗口修正与失败诊断，也不替代实际 AVC、GUI 或 HEVC wire 门禁。见[验证记录](docs/validation/rdp-egfx-h264-20260907.md)和[目标基准](docs/validation/progressive-benchmark-20260908.md)。

2026-09-08 GUI 回归：macOS 控制岛自动隐藏、首次窗口按原生工作区适配和隐藏后的默认 RDP 键鼠输入已受限验证。显式 EGFX 另出现 Progressive region coverage 失败；现在致命图形错误会终止会话并显示错误，严格解码检查仍保留，并已增加仅含帧号/矩形/tile 坐标的失败诊断；真实复现尚待执行，故该实验未验收。见[窗口与输入验证](docs/validation/macos-window-chrome-20260908.md)。

<details>
<summary>2026-09-08 历史调试与构建记录（各段只描述当时提交，不代表当前状态）</summary>

> 2026-09-08 Windows/macOS 打包规则已加入 Progressive 的 Apache-2.0 与 FreeRDP 归属资源，固定文件集合及hash校验同步更新。macOS合成签名包正例/9项拒绝场景及现有debug包临时副本验证通过；新Windows原生Pester与正式重打包仍待CI。

> 2026-09-08 Linux i686/x86_64/ARM64 ClearCodec/NSCodec 各38项测试及2项release基准通过（run `34185964017`）。这是目标进程验证；Progressive与GUI互操作仍须独立验收。见 [基准记录](docs/validation/clearcodec-benchmark-20260908.md)。

> 2026-09-08 最新真实探针（`707b178`）：macOS ARM64 的ClearCodec/Progressive混合流连续刷新20秒，首帧999ms、70帧、2个完整基线，重置后继续更新并正常回收。实际AVC仍为零，H.264与GUI尚未验收；见 [详细证据](docs/validation/rdp-egfx-h264-20260907.md)。

> 2026-09-08 EGFX 最新验证：离屏表面与缓存支持通过 235 项 RDP 测试；真实 Windows 已越过未映射失败点，但在 RemoteFX Progressive `0x0009` 停止，60 秒零帧并正常回收。EGFX/AVC 首帧仍未验收，详见 [验证记录](docs/validation/rdp-egfx-h264-20260907.md)。

> 2026-09-08 用户调整构建范围：macOS 仅构建 ARM64；Intel/x86_64 不支持当前构建与发布，不再是交付门禁。此前 Intel/Rosetta 结果仅保留为历史证据。Windows/Linux 的 x86、x86_64、ARM64 范围不变。

RDP 的有界探针已增加非敏感 EGFX 阶段诊断（187 项协议测试通过），区分能力排队、曾确认和后续失败；新真实会话仍待验证，生产编码门禁不变。

最新 RDP 探针（2026-09-08，`d5140a6`）已收到 EGFX 能力确认，但 AVC420/AVC444 未确认，60 秒内无首帧；已主动断开并回收。现代编码仍为实验性且生产默认关闭，详见 [验证记录](docs/validation/rdp-egfx-h264-20260907.md)。

进一步定位：服务器在该次 EGFX 会话选择 V8.1、未启用 AVC420，并发送当前未处理的 ClearCodec（0x8）。该路径仍为实验性，不能宣称 H.264 登录与画面已验证。

未支持的 EGFX WireToSurface1 编码现在明确记录 `UnsupportedCodec` 并停用当前 generation、丢弃排队更新（189 项 RDP 测试通过）；此错误处理不等于已实现 ClearCodec，也不保证服务端自动恢复 legacy。Windows ARM64 AAC 的 clang-cl 目标配置修复仍待托管完整编译验证。

## 2026-09-08 Windows ARM64 原生解码通过

Windows run `34182980455` 整体成功，ARM64 native runtime job `101929312094` 在真实 ARM64 Windows 上重新验证下载的同次package，并加载随包DLL通过全部6项fixture。连同Windows x86/x64、Linux三架构及macOS ARM64已有记录，固定FFmpeg plugin构建/目标解码门禁已满足；新ClearCodec及真实RDP AVC仍独立未完成。

## 2026-09-08 Windows ARM64 包构建成功

Windows run `34182980455` 的 ARM64 package job `101925633226` 已成功完成完整 AAC、GUI release、PE/package verifier 和上传。ARM64 应用 artifact 为 22,664,457 bytes，对应源码为 11,729,109 bytes。该结果证明目标构建和包检查，后续 `Windows ARM64 native decoder runtime` 尚待依赖 job 完成，不能提前宣称实际 ARM64 DLL 解码通过。

## 2026-09-08 真实定位：映射前 surface 更新

`35fdfb5` 的有界真实 probe 将首次失败精确定位为 `PublisherUnmappedSurface`：服务器在映射前发送 ClearCodec，原发布器的 mapped 前置条件拒绝了合法离屏更新。会话确认 V10.7，60秒无帧后主动断开且cleanup=joined。下一修复是受总预算限制的surface backing/有效区域保存与Map发布，不是放宽矩形检查或跳过序号。

## 2026-09-08 最新真实接线与 ARM64 AAC 结果

`ec5e3c3` 显式 AVC444 probe 再次确认 V10.7/AVC 能力，进入 ClearCodec 路径后 `first_failure=Publisher`，未处理编码计数为0；60秒内无首帧，主动断开且cleanup=joined。故不能把离线225测试通过视为真实首帧通过；正在细分surface布局/发布约束，未证实具体原因前不归咎服务端。另Windows run34182980455的ARM64完整AAC编译gate已成功，GUI build仍在执行。

## 2026-09-08 ClearCodec / NSCodec 接线本地验证

ClearCodec 与完整 NSCodec SIMD provider 已接入显式 EGFX publisher；会话共享 sequence/cache，BGRA 直接进入 BGRX patch，纯缓存重置不伪造像素，失败及发布溢出清空 generation。完整 RDP 225 项测试通过，NSCodec 包含微软官方 15×10 全像素示例。真实首帧、跨目标运行和性能仍待验证；生产默认 LegacyOnly 保持。

## 2026-09-08 ClearCodec 基础状态层验证

ClearCodec 严格状态层、会话级序号/缓存事务与 SIMD 像素内核已注册编译：25 项 ClearCodec/kernel 测试、完整 RDP 214 项通过。含 residual 前缀、bands 位域、RLEX、glyph/cache、零子区域与覆盖元数据预算回归。NSCodec 实际后端和 EGFX 接线仍在开发，因此未打开生产门禁。

## 2026-09-08 Windows x64 完整包门禁通过

Windows run `34181521241` 已结束：x64 job `101921418412` 通过 6 项解码 fixture、GUI/package gate、30/30 Pester 与产物上传；x86 job 同样成功。x64 应用 artifact 为 23,582,300 bytes。ARM64 AAC 编译失败，后续 ARM64 native runtime job 因依赖失败而 skipped，不能算通过。

## 2026-09-08 Windows x86 包与原生解码通过

Windows run `34181521241` 的 x86 job `101921418321` 已成功：真实 i686 进程 6 项 decoder fixture、GUI release、staging/package verifier 与产物上传全部通过。应用包 `freeremotedesk-windows-package-x86` 为 22,263,948 bytes，对应源码包 11,729,109 bytes。此结果不是 Windows GUI 真机交互或 RDP AVC 首帧证据；同轮 ARM64 失败和 x64 仍在运行分别保留。

## 2026-09-08 Linux 三架构目标解码通过

[Linux run 34181521242](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/34181521242) 在 `ca8628e` 上三个架构全部成功：x86_64 原生、i686 32 位目标进程、ARM64 原生各通过 11 项 plugin 单元测试与 6 项随包解码 fixture，覆盖 AVC420、AVC444 和 HEVC 离线样本。ELF/依赖/路径/ABI 与产物上传检查通过。该结果补齐 Linux 目标解码执行证据，仍不证明 Linux GUI 或真实 RDP 编码互操作。

## 2026-09-08 托管 CI 新证据（取代下文 Linux 未执行状态）

[Linux FFmpeg run 34179144122](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/34179144122) 在提交 `6b5191f04e0d891e16063efe93ce04cf1dc05e9f` 上完成：x86_64、i686、AArch64 三个 bundle 的构建、ELF/依赖/路径/ABI 导出检查与产物上传均成功，三个对应源码产物也已上传。x86_64 的原生 fixture 与插件加载/ABI 调用通过；i686/AArch64 的加载步骤明确跳过，host-side tests 不构成目标架构运行证据。Linux GUI、目标机交互和 live RDP 尚未验证，因此平台整体仍为 `受限验证`/`开发中`。

后续目标运行门禁已补充：Windows x86 使用 32 位 fixture 进程，Windows ARM64 使用原生 runner 加载同次构建的包；Linux i686 使用 32 位进程，Linux ARM64 使用原生 runner。新配置尚待托管执行通过，不计为已取得的运行证据。

该运行修复了 YAML 折叠 shell 续行，以及 FFmpeg configuration 嵌入临时 `--prefix` 的真实路径泄露；现使用固定 `/usr` 加 `DESTDIR` staging，未放宽 verifier。Windows ARM64 已改固定 LLVM-MinGW，Windows x86 使用静态 libgcc，实际包结果仍待新 CI。macOS Intel CI 暴露的负色度 SIMD 乘加错误已在 `53d10de` 修复，Rosetta 完整 RDP 186 项通过，ARM64 YUV 回归 7 项通过；此前本机 ARM64 通过不能代表 Intel 正确性。


</details>

FreeRemoteDesk 是纯 Rust 远程登录客户端。当前产品优先实现 Windows
客户端，通过 Apple 原生远程登录服务连接 macOS；后续客户端目标为 macOS、
Linux、Android 和 HarmonyOS NEXT，后续服务端目标为 Windows 原生 RDP 与
Linux 原生 RFB/VNC。项目不要求或部署任何自定义服务端组件。

本文后半部分保留早期 ARP/VNC CLI 的协议说明作为历史实现资料；当前产品
GUI、分层和构建状态以以下矩阵、`AGENTS.md` 及 `docs/superpowers/specs/`
中的现行设计为准。

## 平台与功能实现状态

状态定义：

| 状态 | 含义 |
|---|---|
| **已验证** | 已实现，并在对应客户端与原生服务端组合上完成真机互操作验证 |
| **受限验证** | 已实现并完成有边界的真机验证，但尚未覆盖长时间运行或全部网络条件 |
| **实验性** | 代码存在且默认关闭，协议证据或互操作范围仍不足 |
| **开发中** | 当前分支正在实现，不能作为可交付功能宣称 |
| **计划中** | 架构已预留或列入路线图，尚无可运行实现 |
| **不支持** | 原生协议或产品边界不允许，或已明确决定不实现 |

### 客户端运行平台

| 客户端平台 | GUI/渲染 | 本地输入 | 安装包 | 当前可连接目标 | 状态与证据 |
|---|---|---|---|---|---|
| Windows | winit + egui + wgpu | 键盘、鼠标 | x86_64 默认 package；x86/i686 与 arm64/aarch64 使用同一显式架构 profile 生成独立 codec 目录和 manifest；MSI/MSIX 仍开发中 | macOS；Windows RDP 开发中 | **开发中**；固定 Rust 1.96.0 的协议中立 core/video/plugin crates 已通过 MSVC x86_64/i686/arm64 target check；统一视频 decoder 的编译、离线 fixture、DX12 readback、package staging 与 codec present/absent 单实例 GUI 门禁已完成；`.github/workflows/build-windows.yml` 已扩展 Rust MSVC x86_64/i686/arm64、WSL MinGW、NASM/x86asm 与 AArch64/NEON 的分架构 package/verifier gate，verifier 现在还按 `0x8664`/`0x014c`/`0xAA64` 精确检查 EXE 与 codec DLL 的 PE Machine，`5c00935` Windows run `34192818981` 的三架构包及 ARM64 原生 runtime 已通过；该证据不替代真实 GUI 控制。Apple Standard/HP 与 RDP 的当前真机边界见 [`cross-platform-video-decoder-20260901.md`](docs/validation/cross-platform-video-decoder-20260901.md)；RDP 仍等待独立授权的原生 Windows 目标完成登录、首帧与输入门禁。 |
| macOS | winit + egui + wgpu/Metal | 基本键盘、鼠标已受限验证；完整焦点/滚轮/快捷键仍待验收 | ARM64 ad-hoc signed `.app`；FFmpeg 8.1.2 ARM64 bundle、Info.plist、Mach-O、`--verify-codec-bundle` 和包结构验证通过 | Windows 原生 RDP | **受限验证**；2026-09-07 在一台授权 Windows 目标完成 macOS 原生 GUI 的 TLS/CredSSP/NLA、证书首次记录、完整桌面首帧、断开、Keychain 密码保存和“最近连接”免重新输入密码重连。2026-09-08 ARM64 托管构建、原生 decoder fixture、应用 staging/verifier 与产物上传通过（[run 34180014775](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/34180014775)）；macOS Intel 不支持。2026-09-08 `64dd9e1` 实验EGFX混合流完成真实桌面、开始菜单鼠标/搜索键盘输入和约三分钟正常退出；macOS 控制岛自动隐藏与首次窗口按远程比例适配原生工作区已修正，验证范围见 [窗口几何记录](docs/validation/macos-window-chrome-20260908.md)；完整输入、多DPI/主题、长期运行和公证发布仍未覆盖；见 [`macOS RDP GUI 验证`](docs/validation/macos-native-rdp-gui-20260907.md)。 |
| Linux | Vulkan/GLES 条件后端；独立 GTK/GLArea 窗口探针待原生运行，完整平台 shell 开发中 | 计划中 | i686 完整客户端包及目标解码器加载通过（job `101971932014`）；ARM64/x64 首轮因系统 zlib 清单遗漏失败，修正待复跑 | 尚无已验证的产品组合 | **开发中**；2026-09-08 独立平台服务已有实现、17 项合成测试及三架构 check；应用入口已实现，宿主编译及 20 项单元/2 项边界测试通过；2026-09-08 Ubuntu x86_64 临时 Secret Service 往返及原生应用 20+2 测试已通过（job `101972000994`）；完整包及原生 GUI 仍未验收，见 [基础服务验证](docs/validation/linux-client-foundation-20260908.md)和 [Linux 计划](docs/superpowers/plans/2026-09-08-linux-client.md)。现有 CI 的解码/编译证据不证明窗口管理器、Secret Service、完整安装包或真实控制；这些门禁独立未完成。 |
| Android | Rust 核心边界预留 | 触控/软键盘计划中 | 计划中 | 尚无 | **计划中**；桌面三平台完成后启动，需 Android Keystore 与自适应图标 |
| HarmonyOS NEXT 手机/PC | ArkUI/HUKS 边界设计 | 触控/键鼠计划中 | 计划中 | 尚无 | **计划中**；不是 Android 兼容层，须单独完成 ArkUI、HUKS 和构建 POC |

### 视频解码后端状态

| 客户端平台/路径 | 状态 | 验证范围或阻塞点 |
|---|---|---|
| Windows native capability probe | **受限验证** | 2026-09-01 在单台 AMD Radeon 780M Windows 主机完成 D3D12 profile 探针；Main/Main10 报告 hardware exact，Main444 明确不可用。证据为 [`windows-video-capabilities-20260901.json`](docs/validation/windows-video-capabilities-20260901.json)，仅证明能力探针，不证明 native decoder 或远端会话首帧；Task 10 复跑结果见 [`统一视频解码器验收记录`](docs/validation/cross-platform-video-decoder-20260901.md)。 |
| Windows FFmpeg 8.1.2 Main444 software backend | **受限验证** | 固定签名源码构建的 LGPL 动态插件通过离线 Main444 fixture 精确解码；2026-09-04 Windows x86_64 bundle 已启用 NASM/x86asm，并在 2560x1440 及其竖屏方向使用最多两个 frame threads。新 bundle 通过 Main444、PE imports、manifest、LGPL/对应源码、staging、system-owned 安装器与 trusted-install 门禁；macOS arm64/x86_64 已完成对应 C bridge、plugin 和 native fixture 编译验证，Linux 仍待对应主机执行。证据见 [`Apple HP 延迟验证`](docs/validation/apple-hp-latency-20260904.md)。 |
| RemoteFX Progressive / EGFX `0x0009` | **受限验证** | 2026-09-08 严格 wire/entropy、surface reference、codec DAS 生命周期与 SSE2/NEON 内核已实现。`5c00935` 七目标各通过 Progressive/邻近 EGFX 59 项回归及内核基准，含 Windows ARM64 原生 runtime；这些是离线目标证据。macOS ARM64 混合流 GUI 曾完成桌面及基本输入，但后续实验会话在 47 帧后出现 `region lacks current frame tiles` 并冻结，根因尚未修复。`85324f5` 已将致命图形失败传播为可见连接错误，`7a244c9` 增加有界几何诊断；`4fd7f33` 七目标各 61 项回归及内核基准已通过（Windows run `34195654503`）；实际故障复现仍需验收。默认生产不启用该实验路径。见 [窗口与失败记录](docs/validation/macos-window-chrome-20260908.md)、[目标基准](docs/validation/progressive-benchmark-20260908.md)。 |
| H.264 AVC420/AVC444 FFmpeg software bridge | **开发中** | 2026-09-08 已实现 AVC420/AVC444 wire 校验、双流重建、FFmpeg decoder bridge、SSE4.1/NEON 像素转换及协议中立 SurfaceUpdate 接线。固定 FFmpeg 的 Windows/Linux i686、x86_64、ARM64 与 macOS ARM64 七目标 package/runtime fixture 门禁已有证据；macOS Intel 不支持。真实 Windows 已确认 AVC 能力并以 ClearCodec 产生首帧，但实际 AVC420/AVC444 解码计数仍为0，最新ClearCodec/Progressive混合流已完成20秒持续更新，AVC数据仍未收到；能力确认和混合编码首帧不代表 H.264 互操作完成。生产仍使用 LegacyOnly，只有显式有界探针或 macOS ARM64 `--rdp-egfx-experiment avc420|avc444` 验证入口启用 EGFX；后者在 `64dd9e1` 已取得约三分钟混合流窗口与基本输入证据，实际AVC仍为0。详见 [验证记录](docs/validation/rdp-egfx-h264-20260907.md)。 |
| Apple High Performance 真机首帧与输入 | **受限验证** | 2026-09-04 在一台授权 stock Mac 上完成用户名/密码 HP 会话、认证 RTP、HEVC Main444 软件解码、精确 present、鼠标/键盘输入与持续刷新验证；当前候选以 `0x1d` mode 0 请求并真机确认 2560x1440 pixels / 2560x1440 points / 60Hz（scale 1），初始 Message `0x1c=0x0d`，确认会话内降档时只写一次同几何 30Hz `0x1d`，不重启认证、不发送第二个 `0x1c`。Standard/MVS 保持 `0x1c=0x0c`。当前安装候选 SHA-256 为 `6F3368FE16D05246F54DC6713B0CE7EC3F98B5F508F31AEC2E33305F6DDF8E9A`；20.295 秒受限运动负载记录 246 次呈现，Mac 保持 60Hz；该负载不是持续 60-FPS source，不能作为 decoder 最大吞吐或长期网络结论。证据见 [`Apple HP 延迟验证`](docs/validation/apple-hp-latency-20260904.md)。 |
| macOS / Linux native video backend | **受限验证** | macOS 仅支持 ARM64：FFmpeg builder、stager、verifier 明确拒绝 Intel/x86_64，CI 仅使用 `aarch64-apple-darwin`。2026-09-08 ARM64 package、原生 decoder fixture 和产物门禁通过（run `34182980384`）。Linux i686、x86_64、ARM64 的固定 FFmpeg plugin 原生/32 位进程 fixture 与 ELF/ABI 门禁通过（run `34181521242`）；Linux GUI 包与真机控制仍未验收。历史 macOS Intel/Rosetta 记录不属于当前支持范围。 |
| Android native video backend | **计划中** | 尚无 MediaCodec bridge、移动端 shell 或 package 构建验证。 |
| HarmonyOS NEXT native video backend | **计划中** | 必须单独完成 ArkTS/ArkUI 与 native codec bridge POC；不是 Android 兼容层，当前不冒充 build 支持。 |

### 原生服务端目标

| 服务端系统 | 原生服务 | 客户端协议方向 | 当前客户端 | 总体状态 |
|---|---|---|---|---|
| macOS | Screen Sharing / Remote Management | 两条隔离的 Apple 路线：Standard（`displayType=0` compatibility）与 High Performance（`displayType=1/2` virtual display） | Windows | **开发中**；Standard 使用实体桌面且不创建虚拟显示，但当前 FreeRemoteDesk adapter 尚未实现或注册。High Performance 的 type-1 实体屏幕置黑仅有用户观察，尚不是 Windows 客户端端到端互操作结论。两条路线的当前阻塞、已知观察和禁止回退边界见 [`Apple 双模式阻塞记录`](docs/validation/apple-dual-mode-blockers-20260901.md) |
| Windows | Remote Desktop Services | 独立 `frd-protocol-rdp` + IronRDP 0.17.0 | Windows、macOS | **受限验证**；RDP TLS、CredSSP/NLA、activation、首次自动保存 SHA-256 指纹、相同指纹继续、变化拒绝且不覆盖，以及传统 Bitmap/RemoteFX 首帧与增量已实现。2026-09-07 macOS 原生 GUI 在授权 Windows 目标完成登录、完整桌面显示、断开和 Keychain 保存密码重连；Windows GUI/DPAPI/键鼠本轮仍未完成真机验收，见 [`Windows RDP 验证`](docs/validation/windows-native-rdp.md) 与 [`macOS RDP GUI 验证`](docs/validation/macos-native-rdp-gui-20260907.md) |
| Windows | Remote Desktop Services | 同一 RDP adapter 的有界无 GUI 探针 | macOS arm64（探针） | **受限验证**；2026-09-07 对独立授权原生 Windows 完成首次指纹记录、相同指纹新进程重连、TLS/CredSSP/NLA、activation、1280×720 完整画面及增量解码；第二次观察约 20 秒、133 次更新、主动断开并回收。探针存储仅用于测试，不证明 macOS 产品 GUI、Keychain、键鼠或安装包；见 [`验证范围`](docs/validation/windows-native-rdp.md) |
| Linux | 系统或发行版原生 VNC/RFB 服务 | RFB 3.x 及服务端公开扩展 | 尚无 | **计划中**；不得引入配套守护进程 |

### Windows 客户端连接 macOS 功能明细

本表中既有“已验证/受限验证”记录描述对应认证、MVS、输入或媒体子系统的历史
证据，不自动继承为另一条 Apple 模式的完整互操作结论。Standard（`displayType=0`）
和 High Performance（`displayType=1/2`）必须保持独立；不得借隧道、Standard
降级或既有子系统证据冒充 High Performance。Standard 仍为**开发中**；High
Performance 仅按下表 2026-09-04 的有界范围记为**受限验证**，详见
[`Apple 双模式阻塞记录`](docs/validation/apple-dual-mode-blockers-20260901.md)。

2026-09-04 的当前 Apple HP wire contract 是：初始 scale-1
2560x1440 pixels / 2560x1440 points at 60Hz，Message `0x1c=0x0d`；仅在
已确认 HP session 内，load controller 才可单向写入一次同几何 scale-1
2560x1440/30Hz 的 `0x1d`，不重启认证且不发送第二个 `0x1c`。Standard/MVS
保持 `0x1c=0x0c`。当前安装候选 SHA-256 为
`6F3368FE16D05246F54DC6713B0CE7EC3F98B5F508F31AEC2E33305F6DDF8E9A`；早期
`1280x720` 请求、Mac 选择 `1312x848` 及 bit-clear 候选均已被当前
scale-1 合同取代，仅作为 validation 中的历史证据。

| 功能 | 协议/模块 | 状态 | 验证范围或阻塞点 |
|---|---|---|---|
| Mac 账号密码认证 | Apple HPSS 会话 | **已验证** | 使用 Mac 本地用户名/密码；不请求、保存或使用 Apple ID 凭据 |
| Apple Standard 实体桌面 | `displayType=0` compatibility；不发 HP 虚拟显示配置 | **开发中** | 使用 Mac 实体桌面、无虚拟显示；当前 FreeRemoteDesk adapter 尚未实现/注册，不能由现有 HPSS/MVS 代码或登录成功替代。须先取得并落实该模式的会话选择、认证、codec-6/经典 RFB 首帧和持续更新证据，见 [`双模式记录`](docs/validation/apple-dual-mode-blockers-20260901.md) |
| High Performance 虚拟显示与实体显示器置黑 | `displayType=1/2` virtual display；`0x3f2`/`0x1c` + SRTP/SRTCP + HEVC RTP | **受限验证** | 2026-09-04 在一台授权 stock Mac 上完成用户名/密码 HP 会话、认证 RTP、HEVC Main444 软件解码、精确 present、鼠标/键盘输入与持续刷新验证。当前候选真机确认 mode 0 为 2560x1440 pixels / 2560x1440 points / 60Hz（scale 1），初始 `0x1c=0x0d`；已确认会话内降档只写一次同几何 30Hz `0x1d`，不重启认证、不发送第二个 `0x1c`，Standard/MVS 保持 `0x1c=0x0c`。Windows 使用 x86asm + 最多两个 FFmpeg frame threads；受限运动负载后 Mac 仍保持 60Hz，未触发降档；尚未覆盖持续 60-FPS source、最大 decoder throughput、live 30-Hz fallback、任意网络、长期运行或 dynamic resize，实体显示器置黑仍仅是用户观察。当前安装候选 SHA-256 为 `6F3368FE16D05246F54DC6713B0CE7EC3F98B5F508F31AEC2E33305F6DDF8E9A`。证据见 [`Apple HP 延迟验证`](docs/validation/apple-hp-latency-20260904.md) 与 [`双模式记录`](docs/validation/apple-dual-mode-blockers-20260901.md)。 |
| 完整桌面画面 | Apple HPSS + MVS type-0/type-1 | **已验证** | 2026-08-31 保留固定捕获仅证明采样候选 `c57dc77` 的 Windows wgpu frame-transaction 路径在有界 Apple HPSS/MVS 真机比较中通过；范围、run id 与二进制身份见 [`windows-apple-wgpu-parity.md`](docs/validation/windows-apple-wgpu-parity.md)。这不证明 Standard adapter，也不证明严格 High Performance 虚拟显示/实体显示器置黑与恢复门禁。 |
| 增量桌面更新 | ARD 3.10 MVS type-1 | **已验证** | 严格回放 18 条记录并完成有界真机更新；type-1 原位更新持久 CPU surface，只发布 MVS dirty rect，mailbox 不克隆像素，wgpu 只上传对应矩形。`3e375c0` 还把超过 32 个稀疏 dirty rect 确定性分为最多 32 个局部 patch，禁止退化为近整屏的全局包围矩形。2026-08-31 的 `c57dc77` 采样候选通过固定真机比较；后置修复只有离线证据，范围与非结论见 [`windows-apple-wgpu-parity.md`](docs/validation/windows-apple-wgpu-parity.md)。 |
| 鼠标输入 | Apple 会话输入消息 | **已验证** | 仅窗口与远程内容具备所需焦点时发送；移出窗口不继续注入 |
| 键盘输入 | Apple 会话输入消息 | **已验证** | 基础按键与修饰键已真机验证；平台 IME 完整适配仍需单列验证 |
| 动态分辨率 | 实验性 resized `0x09` + generation 切换 | **实验性** | 默认关闭；离线 eligibility 只接受匹配初始 `ServerState` 的 `display_count=1`，`display_count=2` 仅关闭 dynamic、不会关闭已确认的 High Performance 或 publisher；尚无足够 Apple 线协议互操作证据，见 `AGENTS.md` P1 |
| Mac→Windows 音频 | Apple UDP/SRTP + AAC-ELD | **受限验证** | 已认证、解码非静音 48 kHz 双声道并通过 Windows 输出；未宣称任意丢包与长时间 rollover |
| UDP 媒体传输 | Apple Message 1/2、`0x1c`、SRTP/SRTCP | **受限验证** | 音频和视频 socket 已完成有界真机互操作；长时间网络稳定性未覆盖 |
| Windows→Mac 麦克风 | Apple Audio Chat / IDS 路径 | **不支持** | 原生用户名密码 HPSS 会话没有已恢复的 Audio Chat 分支；Apple ID 与服务端助手均超出产品边界 |
| 剪贴板 | 能力边界已预留 | **计划中** | 当前 Windows 产品未完成端到端剪贴板集成 |
| 动态保存登录信息 | Windows Credential Manager + 非敏感配置 | **开发中** | Windows 客户端的自动化状态机、非敏感元数据及本机凭据库往返已通过，但 Windows 客户端连接原生目标的 GUI 真机提交/重连仍未验收；macOS 客户端的 Keychain GUI 结果单列于 [`macOS RDP GUI 验证`](docs/validation/macos-native-rdp-gui-20260907.md)。 |
| 文件传输 | 未选择 | **计划中** | 需先确认各原生服务端支持的协议与安全边界 |

当前桌面 frame port 以自身真实 64 MiB 预算签发不透明 generation admission。
Apple startup、初始确认、动态 viewport 请求、精确 ACK 和服务端 geometry 路径都
必须在 CPU surface 分配/替换、resized/full wire 写入、私有状态修改以及 generation
event、`Reset`、wake 之前取得 admission；失败为 terminal，初始路径不留下部分
状态，已有动态 surface 与控制状态原子保留。`4a080cf` 与 wrapper 修复 `af57bd5`
共同关闭 admission 所有权边界：跨 runtime、stale 或其他无效 opaque token 会先 poison 目标 runtime，
再返回 `InvalidGeneration`，且本次拒绝不新增 event、`Reset` 或 wake；直接传给
`admit_generation` 的错误输入仍只返回 `InvalidGeneration`，不会 poison，可恢复。
`8a32038` 只把 dynamic eligibility 收紧到单显示器，不把两显示器配置误判为
High Performance 失败。

`aa560a3` 已把项目自有热路径中的 `wgpu::Device`、fault observer handle 与
`GpuContext` clone 降为零，但这不是“绝对零 GPU clone”：wgpu 30 的每次
`push_error_scope` 会在内部 clone 一个 `DispatchDevice`，每个 FRD fault scope
调用三次；常规呈现的 acquisition、frame、record 合计 9 次上游 clone，
`CandidateBatch` 另有 3 次。这组固定上游 clone 被接受，不能归入项目自有
handle-clone 回归。

这些 runtime/GPU 修复（含 `42194bc` 的 batch 双 panic 收敛）和当前 comparator 提交链
`cd01e78`、`18498ed`、`dec2cd6`、`3bde039`、`3db3b53`、`50354fa`、`f7339cd`
都发生在 Task 7 真机采样之后。唯一 Mac live 二进制仍是 serial `44a62ad`
（SHA-256 `8CED2D0DB0788D34152AE498461A18F0255B103B3C20F87FCD2026932DD4C421`）
与 candidate `c57dc77`
（SHA-256 `4D1AECB691463E813F3C36122C9BC83464BB697028113C7AFE5814A0F102207F`）。
`50354fa` 仅重放相同 retained CSV：Windows PowerShell 5.1 输出 15,888 B、
SHA-256 `B6E8B860618E83DA0F851D4AC1A337ACA00F04CB47E16A950CFD5EB3FFF2AD64`；
pwsh 7 输出 6,354 B、SHA-256
`7651B13855D77C2D87BC805ABE4FC3D80B9750ACB8C68DBCB635F337831C850A`。
两者仅 JSON 空白格式不同，解析后语义相等且均为 14/14；`f7339cd` 进一步校验
process 数值规范与逐秒 CPU delta，并隐藏后台 PowerShell 窗口；`2520452` 让采集器
以 UTF-8 BOM/CRLF 同时兼容 Windows PowerShell 5.1 与 pwsh 7。它们都不是 recapture。
`43db868` 的 42,283,520 B / `AFDB...AE69` 只是先前离线闭包构建，不能冒充
当前最终构建或替换上述采样哈希。`183b99d` 把 Windows Release 改为 GUI subsystem，
正常启动不再附带 console，fatal 由原生错误对话框显示。当前离线闭包 Release 为
42,292,736 B，SHA-256 `21D307C64C6C153F4592FD1B1DC0C20856868F6DE239EDDD8DE087B6B894F6FC`，
PE subsystem 2；这是构建证据，不是新的 Mac 互操作证据。

### Windows 客户端连接 Windows 功能明细

| 功能 | 协议/模块 | 状态 | 验证范围或阻塞点 |
|---|---|---|---|
| 服务器身份与 TLS | RDP TLS + 首次使用信任 + SHA-256 pin | **开发中** | 2026-09-07 首次自动持久化指纹再继续认证；自签名和无 IP SAN 可连接，相同指纹自动继续，变化停止且不覆盖旧记录；保留证书有效期、用途、格式、握手签名与预检后换证校验。macOS 无 GUI 探针已真机通过首次与重连，Windows 产品 GUI/DPAPI 真机门禁仍未完成；见 [`证据`](docs/validation/windows-native-rdp.md) |
| 账号密码认证 | CredSSP/NLA | **开发中** | 私有 adapter 已实现只允许 NLA/TLS 的 CredSSP、licensing 与 activation 基线；2026-09-07 macOS 无 GUI 探针已完成 Windows 目标认证与激活；Windows 产品客户端尚无本轮真机登录证明。凭据不得进入 argv、普通配置、日志或抓包 |
| 基础桌面画面 | Raw、Interleaved RLE、RDP 6 Bitmap、RemoteFX | **开发中** | 已有 `freeremotedesk-windows` Release 构建，设计为 BGRX 脏矩形发布；2026-09-07 macOS 无 GUI 探针已解码 Windows 目标完整画面；Windows 产品窗口呈现仍未验收 |
| 鼠标与键盘 | RDP fast-path input | **开发中** | 已有离线 scan code、物理修饰键、Unicode、鼠标、滚轮和失焦 `ReleaseAll` 覆盖；当前协议中立 `Modifiers` 没有 Caps/Num/Scroll Lock 状态位，锁定状态同步明确延期且本分支不新增公共输入/UI schema；尚未完成产品互操作 |
| 动态分辨率与多显示器 | Display Control DVC | **开发中** | 已通过现有 viewport 接口实现单主显示器 latest-only 调整，仅在 DVC 打开且服务端能力就绪后宣告，并在精确 reactivation 尺寸确认后切换 generation；多显示器仍不支持。2026-08-29 证据仅限单元/workspace 测试，无 Windows 真机互操作证明 |
| 本地分辨率优先级 | `DisplayIntent` + 物理 viewport | **开发中** | 桌面 shell 默认以当前显示器物理像素规划初始 DesktopSize，允许超过 2560×1440；协议最大尺寸、帧预算和服务端 Display Control 能力决定最终收敛结果。 |
| 现代图形 | EGFX、ZGFX、AVC/AVC420、AVC444、HEVC | **开发中** | 已固定 IronRDP 0.17 的 EGFX DVC 接缝、完成 H.264 AVC420 codec-neutral/FFmpeg bridge 离线门禁，并建立 H.264 AVC444/YUV444P8 的独立 FFmpeg backend contract；同时加入 handler 到 generation-bound `SurfaceUpdate` 队列、共享 NEON/SSSE3 BGRX leaf kernel 及 Reset→runtime generation 提交边界验证。由于 pinned IronRDP connector 默认漏发 `SUPPORT_DYN_VC_GFX_PROTOCOL`，根目录现在以 `third_party/ironrdp-connector-0.10.0` 提供最小本地补丁：只有精确 decoder provider 且显式 `ValidationOnly`（验证）或 `LiveInteroperable`（已有生产证据）时才设置该 wire bit，legacy 默认保持关闭，并由 MCS Connect Initial 重解析测试覆盖。`RdpProtocolFactory::with_egfx_decoder_provider` 与 `Avc420DecoderProvider` 已提供应用组合根注入边界，要求精确 media factory 与首个 access unit 的 SPS/PPS；默认组合根只准备 provider，不广告 EGFX，bounded probe 通过显式 `ValidationOnly` gate 进行观察，不会误报 AVC444；Windows/macOS 组合根只在固定 FFmpeg bundle 能力精确匹配时准备 AVC420 provider，失败仍使用 Bitmap/RemoteFX。ResetGraphics 尺寸若不同于 decoder coded size 时会在发布 runtime Reset 前 fail-closed；pinned decoder reset health gate 失败时也会在通知 handler 前停止并清空 EGFX publisher，保持 legacy surface 不变；EGFX reset 还会同步重绑 legacy generation tracker，显示重激活提交新代际时则关闭仍绑定旧代际的 EGFX publisher，保证 Bitmap/RemoteFX 回退继续可用。graphics observer 额外报告 `egfx_frame_confirmed`，只有实际 EGFX `FrameBoundary` 成功发布到 runtime 后才置位，从而把能力确认与真实 EGFX 画面证据分开。AVC420 wire 现在同时兼容 Annex-B/length-prefix，并按规范定义的排他坐标 `regionRects` mask 逐区域发布解码 patch；quant/quality metadata 不参与解码。macOS arm64 bundle 已通过 H.264+HEVC 包门禁，opt-in probe 已验证本地 provider 加载但未取得可审计 EGFX surface 证据，故真实 AVC420 首帧和生产选择仍 fail-closed。AVC444 envelope 已严格解析，显式 `Avc444Decoder` 接缝会在安装 decoder 时按两个子流 `regionRects` 并集逐区域完成 WireToSurface1→RGBA→BGRX/SurfaceUpdate 分发，并以独立参考模块覆盖 V1/V2 双流区域组合、色彩转换和事务性边界测试，但 AVC444 production provider、服务器确认、真实 H.264/AVC444/HEVC 互操作、Windows/Linux native fixture 仍未完成。2026-09-08 对照 [MS-RDPEGFX WireToSurface1 codec ID 表](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/fb919fce-cc97-4d2b-8cf5-a737a00ef1a6)，公开标准列出 AVC420、AVC444 和 AVC444V2，但没有 HEVC codec ID；[Azure Virtual Desktop 图形编码说明](https://learn.microsoft.com/en-us/azure/virtual-desktop/graphics-encoding) 只说明特定 GPU 场景可使用 HEVC，没有给出可用于本客户端的 MS-RDPEGFX wire profile。因此 HEVC 仍保持 fail-closed，必须先取得精确产品协议/抓包证据；新增选择器只有在精确 wire profile、decoder、生产互操作和当前协商同时成立时才让 HEVC 排在首位。实现顺序为 AVC420、RemoteFX 优化、AVC444、HEVC；详见 [`RDP EGFX/H.264 验证记录`](docs/validation/rdp-egfx-h264-20260907.md)。未来只允许作为 RDP adapter 内部解码路径发布现有 `SurfaceUpdate`，不新增 UI |
| 文本剪贴板 | CLIPRDR | **开发中** | CLIPRDR 仅在私有 RDP adapter 内适配 Unicode 文本，并由现有协商能力作产品门控；Windows 平台剪贴板 gate 本分支未启用，文件能力保持关闭，不能宣称端到端剪贴板。2026-08-29 证据仅限 adapter 单元/workspace 测试，无 Windows 真机互操作证明 |
| Windows→客户端音频 | RDPSND | **开发中** | 已将共同协商的 48 kHz 双声道 16-bit PCM 通过协议中立 `MediaFrame` 端口发布；`wFormatNo` 按客户端公布的共同格式列表索引验证，媒体背压只降级音频，RDP adapter 不打开平台音频设备。2026-08-29 证据仅限单元/workspace 测试，无 Windows 真机互操作证明 |
| 客户端麦克风、文件、磁盘与设备 | RDPEAI、CLIPRDR 文件、RDPDR | **计划中** | 不在当前 RDP 开发范围，也不新增入口或公共接口；未来必须单独设计并获得批准 |

当前 RDP 开发只适配 FreeRemoteDesk 已有的统一登录、证书身份记录、画面、键鼠、
动态分辨率、文本剪贴板、远程音频、状态与断开接口，以及实现这些接口所必需的
IronRDP 协议内部要求。IronRDP 的其他能力不属于当前 roadmap；不得为其扩展当前
UI、公共接口或平台服务，RDP adapter 的本地改动也不得改变或门控 Apple
HPSS/ARD/MVS 路径。

### macOS 客户端连接 Windows 功能明细

| 功能 | 协议/模块 | 状态 | 验证范围或阻塞点 |
|---|---|---|---|
| 原生窗口与远程内容布局 | macOS winit + egui/Metal shell | **受限验证** | 2026-09-07 staged `.app` 保留 traffic lights，控制岛位于标题栏区域，远程桌面占据内容区；包结构、Info.plist、arm64 Mach-O、FFmpeg bundle 和 ad-hoc 签名通过。 |
| 服务器身份与 TLS | RDP TLS + SHA-256 TOFU pin | **受限验证** | 首次连接显示自签名证书并保存指纹；相同指纹继续；变化时停止自动连接、不覆盖旧 pin。授权目标指纹与主体详情在连接状态栏显示；换证 fail-closed 由离线测试覆盖。 |
| 账号密码认证与完整桌面 | CredSSP/NLA + Bitmap/RemoteFX + baseline compiler | **受限验证** | 2026-09-07 使用授权 Windows 目标完成 macOS GUI 登录，显示实际 Windows PowerShell 桌面；修复了首批 RDP 局部 bitmap 在完整覆盖前被误当作全屏 baseline 的黑屏问题。 |
| 保存登录信息 | macOS Keychain + 独立非敏感 profile metadata | **受限验证** | 勾选“在此设备上保存登录信息”后，TransportReady 在后台提交凭据；断开后从“最近连接”选择记录，密码字段自动恢复为隐藏字符并成功重连。Keychain 读取和提交不阻塞窗口；锁定/授权失败会保留表单并提示重新输入。 |
| 键盘、鼠标、长时间稳定性 | RDP fast-path input / session lifecycle | **开发中** | 2026-09-08 `64dd9e1` 实验EGFX混合流观察到开始菜单鼠标、搜索键盘输入、Escape返回及正常断开；约三分钟末次诊断110帧/失败0。完整快捷键、焦点、滚轮、长期刷新和多目标仍未验收，见 [`最新证据`](docs/validation/rdp-egfx-h264-20260907.md)。 |
| 远程分辨率选择 | 协议无关 `DisplayIntent` + 本地显示器物理像素规划；RDP Display Control | **开发中** | 默认优先当前显示器物理分辨率，支持工作区、窗口内容、固定尺寸（含 3840×2160 以上）和服务器管理；IronRDP 16 位字段与 64 MiB 帧预算只作为实际约束，不设 2560×1440 产品上限。高分辨率实机互操作尚未单独验收。 |

### 客户端与服务端组合

| 客户端 \ 服务端 | macOS 原生服务 | Windows 原生服务 | Linux 原生服务 |
|---|---|---|---|
| Windows | **开发中** | **开发中** | **计划中** |
| macOS 产品客户端 | **受限验证**；2026-09-07 原生 GUI 完成登录、证书 pin、完整桌面、断开与 Keychain 密码重连，见[`证据`](docs/validation/macos-native-rdp-gui-20260907.md) | **受限验证**；同一 GUI 实测 Windows RDP，输入、长时稳定性和多目标覆盖未完成 | **计划中** |
| macOS 无 GUI RDP 探针 | **计划中** | **受限验证**；2026-09-07 首次指纹、重连、认证和完整画面解码，见[证据](docs/validation/windows-native-rdp.md) | **计划中** |
| Linux | **计划中** | **计划中** | **计划中** |
| Android | **计划中** | **计划中** | **计划中** |
| HarmonyOS NEXT 手机/PC | **计划中** | **计划中** | **计划中** |

矩阵只记录已完成的实际层级：编译通过、安装包生成、客户端本地运行、协议
实现和真机互操作必须分别验证，不能相互替代。任何新增功能或平台改动都必须
在同一提交中更新本节。

本地可复现的无凭据验证入口是
[`tools/verify-rdp-egfx-local.sh`](tools/verify-rdp-egfx-local.sh)。它固定 Rust 1.96.0，
运行格式、Shell 语法、协议/媒体 focused tests、workspace tests、七个
Windows/Linux/macOS core/video/plugin target checks、顶层与 `hpssview` CLI help，并在已有产物时调用 macOS package verifier。
缺少工具链、package 或 native FFmpeg fixture 会明确显示为未执行；设置
`FRD_VERIFY_STRICT=1` 才会把这类输入缺失变成失败。该脚本不会读取凭据或执行 RDP live
连接，因此 AVC420、AVC444、HEVC 的服务器协商、首帧、持续刷新和恢复仍须单独记录。
Linux 主机还可设置 `FRD_LINUX_FFMPEG_BUNDLE` 和
`FRD_LINUX_FFMPEG_PROFILE`（`linux-x86_64`、`linux-x86` 或 `linux-aarch64`）调用
Linux ELF/ABI bundle verifier。
macOS 仅验收 `aarch64-apple-darwin` 原生 fixture；Intel/Rosetta 不属于后续构建或验收范围。

### 近期待办

- **Apple 动态 resize（P1）**：保持默认关闭和 **实验性**。以 ARD 3.10
  运行证据为唯一协议基准，补齐 resized `0x09` 的真机互操作门禁；确认服务端
  精确接受新尺寸后，再以一个原子 generation 切换同步替换 surface 尺寸、MVS
  decoder 状态、wgpu texture 与输入坐标变换，并要求新 generation 的完整
  type-0 baseline。即使 High Performance 已在两显示器配置下确认，dynamic
  eligibility 仍只允许 `display_count=1`；没有该证据前不得用本地缩放或自定义
  协议冒充远端 resize。

---

## 一、ARP 协议分析

### 1.1 ARP 解决什么问题

以太网（含 Wi-Fi）上两台主机通信，最终必须知道对方的 **MAC 地址**（链路层地址），而应用程序只知道 **IP 地址**（网络层地址）。ARP（Address Resolution Protocol，RFC 826）就是同一链路（广播域）内 "IPv4 → MAC" 的解析协议。

> 你用 RDM 远程 Mac mini 时：Windows 已通过 ARP 把 `192.168.x.x`（Mac mini 的 IP）解析成 `F0:18:98:…`（Mac mini 的 MAC），之后的 RDP/VNC TCP 报文才能封装进以太网帧发出去。**ARP 是所有局域网通信的前置步骤。**

### 1.2 报文结构（以太网 + IPv4 时固定 28 字节）

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|   硬件类型 HTYPE = 1 (以太网)   |  协议类型 PTYPE = 0x0800 (IPv4) |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
| 硬件地址长度 | 协议地址长度 |      操作码 OP: 1=请求 2=应答      |
|   HLEN=6    |   PLEN=4    |                                    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|           发送方硬件地址 SHA（6 字节，询问者/应答者的 MAC）       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|           发送方协议地址 SPA（4 字节，询问者/应答者的 IP）        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|           目标硬件地址 THA（6 字节，请求中为全 0，应答中填充）     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|           目标协议地址 TPA（4 字节，想解析的 IP）                 |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

一个真实的 ARP 请求（"who-has 192.168.1.20 (192.168.1.10)"）在链路上的样子：

```
以太网帧头: ff:ff:ff:ff:ff:ff | 0a:1b:2c:3d:4e:5f | 0x0806 (类型=ARP)
ARP 载荷:
  0001 0800 06 04 0001        <- HTYPE/PTYPE/HLEN/PLEN/OP=1(请求)
  0a1b2c3d4e5f                <- SHA: 发送者 MAC
  c0a8010a                    <- SPA: 192.168.1.10
  000000000000                <- THA: 未知，填 0
  c0a80114                    <- TPA: 192.168.1.20（想问的目标）
```

ARP 应答则把 OP 置 2，THA 填上自己的真实 MAC，SPA 是被询问的 IP：
`192.168.1.20 is-at f0:18:98:aa:bb:cc`。**注意应答是单播**，只发回给询问者。

### 1.3 工作机制

1. 主机 A 要向同网段的 B（IP 已知）发包，先查本地 ARP 缓存（Windows 命令 `arp -a`）；
2. 缓存未命中 → A **广播** ARP 请求（目的 MAC = `FF:FF:FF:FF:FF:FF`），广播域内所有设备都会收到；
3. 只有 IP 与 TPA 匹配的 B **单播**应答，其余主机静默丢弃；
4. 双方各自把对方写入 ARP 缓存（Windows 未使用条目约数十秒后老化；macOS 约 20 秒）；
5. **免费 ARP（Gratuitous ARP）**：SPA == TPA 的请求，用于通告/检测 IP 冲突——macOS 和 Windows 开机或换网时都会发。

### 1.4 本工具的 ARP 实现方式（`src/arp.rs`）

用户态直接构造/嗅探原始以太网帧需要 Npcap 驱动 + 管理员权限。本工具改用 Windows 官方路径：

| 需求 | 实现手段 |
|---|---|
| 发 ARP 请求并等待应答（"ARP ping"） | `iphlpapi!SendARP(ip, …)`，内核代发真实 ARP 报文 |
| 确定本机网段 | UDP `connect()` 技巧：向公网地址 connect 一个 UDP socket 不发包，仅让内核选路由，然后 `local_addr()` 读出本机出口 IP |
| 高速扫描 /24 | SendARP 对离线主机会阻塞到内核超时，用 128 线程并发摊平延迟 |
| 识别 Apple 设备 | MAC 地址前 3 字节（OUI）比对内置的 Apple 前缀表 |
| 确认是 VNC 服务端 | 对存活主机并发 TCP 连接 5900，读 12 字节 banner，`RFB 003.008` 即 VNC |

`SendARP` 的 IP 参数按网络字节序传递：数值上等于 `u32::from_le_bytes(ip.octets())`。

### 1.5 安全视角（防御性说明）

- ARP 无任何认证：广播域内任何主机都可伪造应答（**ARP 欺骗/中间人**）。
- 防护：动态 ARP 检测（交换机 DAI）、静态绑定、加密上层协议（TLS/SSH）。
- 本工具仅发送标准 ARP **请求**用于自有局域网设备发现，不实现任何欺骗功能。

---

## 二、VNC / RFB 协议分析（RFC 6143）

macOS 的"屏幕共享"内置于系统（ leopard 以来），同时兼容标准 VNC 客户端。协议为 RFB（Remote Framebuffer）3.8。

### 2.1 连接生命周期

```
客户端                                服务器 (macOS:5900)
   | << "RFB 003.008\n"                1. 版本握手
   | >> "RFB 003.008\n"
   | << u8 数量 + 安全类型列表          2. 安全协商
   | >> u8 选择的类型 (2 = VNC Auth)
   | << 16 字节随机挑战                 3. DES 挑战-响应
   | >> DES(挑战, 密码派生密钥) 16 字节
   | << u32 0 (成功) / 1+原因 (失败)
   | >> ClientInit(共享标志=1)          4. 初始化
   | << ServerInit(宽, 高, 像素格式, 桌面名)
   | >> SetPixelFormat / SetEncodings   5. 会话
   | >> FramebufferUpdateRequest
   | << FramebufferUpdate(矩形列表)     … 循环
   | >> KeyEvent / PointerEvent         （键鼠输入随时穿插）
```

macOS 的安全类型列表通常是 `[30, 2]`：
- **30 = Apple Remote Desktop**：Apple 私有的 Diffie-Hellman 认证（RDM 等商业客户端用它）；
- **2 = VNC Authentication**：标准 DES 挑战-响应。**只有在 Mac 上勾选了"VNC 显示程序可以使用密码控制屏幕"并设置密码后才会出现**。本工具使用类型 2。

### 2.2 VNC DES 认证细节（`src/vnc/auth.rs`）

1. 密码截断/补零到 8 字节——**所以标准 VNC 密码只有前 8 位有效**；
2. 每个密钥字节**按位反转**：VNC 的 DES 采用 LSB-first 密钥约定，与 FIPS 46 标准 DES 的 MSB-first 恰好互为镜像；
3. 用该密钥以 **ECB 模式**加密 16 字节挑战（两个块），回传。

正确性由 RFC 6143 附录 B 的测试向量保证（`cargo test` 可验证）。

### 2.3 像素格式策略

客户端通过 `SetPixelFormat` 告诉服务器"按这个格式发像素"。本工具声明：

```
32bpp / depth 24 / 小端 / 真彩色 / R<<16 | G<<8 | B（max 全 255）
```

于是 Raw 矩形的每 4 字节按小端读出 u32 即 `0x00RRGGBB`，与帧缓冲内部格式、minifb 窗口缓冲格式完全一致，零换算直通渲染。

### 2.4 帧编码

本工具请求并实现 `Raw(0)` + `CopyRect(1)`：
- **Raw**：矩形原始像素流，协议强制所有服务器支持；
- **CopyRect**：屏幕滚动时服务器只发"把 (sx,sy) 的矩形搬到 (x,y)"，高效且无损；
- Hextile/ZRLE/Tight 等压缩编码未实现（Raw 在局域网内带宽已足够）。

### 2.5 服务器消息类型

| 类型 | 含义 | 处理 |
|---|---|---|
| 0 | FramebufferUpdate | 逐矩形解码写入帧缓冲 |
| 1 | SetColourMapEntries | 真彩色会话按协议跳过 |
| 2 | Bell | 响铃提示 |
| 3 | ServerCutText | 远端剪贴板文本（打印到终端） |

客户端消息：`SetPixelFormat(0)` `SetEncodings(2)` `FramebufferUpdateRequest(3)` `KeyEvent(4, X11 keysym)` `PointerEvent(5, 位掩码按键+坐标)`。

---

## 三、使用方法

### 3.1 macOS 侧（被控端）开启屏幕共享

系统设置 → 通用 → 共享 → 打开 **屏幕共享** → 点 ⓘ → 勾选 **"VNC 显示程序可以使用密码控制屏幕"** → 设置密码（**前 8 位有效**）。若开启防火墙，允许"屏幕共享"。

### 3.2 Windows 侧（本工具）

```powershell
cargo build --release

# 1. ARP 扫描局域网，自动找出开了 VNC 的设备
.\target\release\freeremotedesk.exe scan
# 2. 查看服务器信息（不带密码可看协议版本和认证方式）
.\target\release\freeremotedesk.exe info <host>

# 3. 先通过非回显环境/凭据提供器设置 FRD_PASSWORD；
#    Apple 账号认证还需设置 FRD_USERNAME。凭据不会进入进程命令行。
.\target\release\freeremotedesk.exe shot <host> -o mac.png

# 4. 实时窗口 + 键鼠控制（Ctrl+Q 退出；--scale 0.5 可缩小窗口）
.\target\release\freeremotedesk.exe view <host>
```

参数细节见 `--help`。`scan` 支持 `--cidr <network>/<prefix>` 手动指定网段。

### 3.3 常见问题

| 现象 | 原因与解决 |
|---|---|
| 提示"只提供 Apple Remote Desktop 的 DH 认证" | Mac 未启用 VNC 密码，按 3.1 设置 |
| 认证失败 | 密码错误；注意仅前 8 位有效 |
| 扫不到 Mac | 确认同一网段；Wi-Fi 隔离会阻挡 ARP；用 `--cidr` 指定 |
| 画面颜色异常 | 极少数服务器忽略客户端字节序声明（本工具未遇到） |

---

## 四、项目结构

```
src/
├── main.rs           CLI（scan / info / shot / view）
├── arp.rs            ARP 发现：SendARP、/24 并发扫描、OUI 识别、5900 探测
├── framebuffer.rs    帧缓冲（Raw 写入 / CopyRect 搬移 / PNG 导出）
├── keysym.rs         minifb 按键 → X11 keysym 映射
├── viewer.rs         实时查看器（读线程 + minifb 窗口 + 键鼠回传）
└── vnc/
    ├── protocol.rs   RFB 常量、像素格式、客户端消息编码
    ├── auth.rs       VNC DES 认证（含 RFC 6143 测试向量）
    └── client.rs     握手/协商/认证/服务器消息解析
```

## 五、已知限制

- 帧编码仅 Raw + CopyRect（局域网足够；公网建议后续加 ZRLE/Tight）；
- minifb 无 IME，无法向远端输入中文（键码层面完整支持英文/功能键/小键盘/修饰键）；
- ARD 认证**已实现三种**（凭据均为 Mac 真实账号，提供 `-u` 时自动优选 36）：
  - **类型 33 RSA-SRP**（`src/vnc/rsa_srp.rs`，2026-08 逆向）：RSA-2048 PKCS#1 v1.5
    包裹的 SRP（Apple 客户端原生默认路径），内层 SRP 与 36 同构；
  - **类型 36 SRP-6a**（`src/vnc/srp.rs`，2026-08 逆向）：corecrypto SRP-6a
    （RFC 5054 4096 组 + SHA-512 + PBKDF2 预哈希），含 H_AMK 服务器证明校验，
    字节级流程见 `docs/ARD_PROTOCOL.md` §5.0；
  - **类型 30 DH**（`src/vnc/ard.rs`）：服务器下发 g/keyLen/模数/公钥（macOS 26
    实测为 RFC 5054 的 4096-bit 组、g=5），客户端回
    AES-128-ECB(MD5(共享密钥), 用户名[64]||密码[64]) + DH 公钥；
  35（Kerberos）与 MVS 编码尚未实现（逆向资料见 `docs/ARD_PROTOCOL.md`）；
- 仅限 Windows（SendARP）；仅可用于自有/授权网络与设备。

## 六、测试与调试

- `cargo test` 覆盖三部分：DES 密钥派生权威向量（密码 `"COW"` → 密钥 `C2 F2 EA…`）、
  ECB 加解密往返、以及内置模拟 macOS 的 RFB 服务器的**端到端集成测试**
  （版本握手 → 安全协商 → DES 认证 → ServerInit → Raw/CopyRect 更新 → PNG 导出）。
- 本机调试凭据存放在 `CREDENTIALS.local.md`（已加入 `.gitignore`，不会入库）。
