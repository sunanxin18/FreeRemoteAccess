# Windows 原生 RDP 验证记录（更新于 2026-09-08）

## 范围与结论

本记录覆盖历史 Windows 客户端离线门禁，以及 2026-09-07 macOS arm64 无 GUI
探针对独立原生 Windows 目标的有界互操作。最新 TOFU 候选已完成首次指纹保存、
相同指纹重连、TLS/CredSSP/NLA、activation 与完整画面解码。
Windows 产品 GUI、键鼠和窗口呈现未在本轮真机验收，产品状态仍为 **开发中**；
macOS 无 GUI adapter 的以下具体范围为 **受限验证**。

## 当前图形能力边界

当前 RDP adapter 在没有精确 decoder provider 时只启用传统 Bitmap/RemoteFX 图形基线。
非敏感 `RdpGraphicsCapability` 诊断在该默认路径记录 `legacy_bitmap=true`、
`remotefx=true`，并将 `egfx_advertised`、`egfx_confirmed`、`avc420` 与 `avc444` 设为
`false`。Windows/macOS 组合根在固定 FFmpeg bundle 能力精确匹配时可以显式注入
AVC420 provider；这条路径会广告 EGFX，但仍必须等待服务器 `CapabilitiesConfirm` 和
真实首帧、刷新、恢复证据。本记录中的 Bitmap/RemoteFX 结果不构成现代图形编码的互操作
证据，HEVC 仍未接入 RDP connector。

标准协议复核也保持这一边界：公开的 [MS-RDPEGFX `RDPGFX_WIRE_TO_SURFACE_PDU_1` codec ID 表](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/fb919fce-cc97-4d2b-8cf5-a737a00ef1a6)列出 AVC420、AVC444 和 AVC444V2，但没有 HEVC codec ID；[Azure Virtual Desktop 图形编码说明](https://learn.microsoft.com/en-us/azure/virtual-desktop/graphics-encoding) 对 HEVC 的描述属于特定 GPU 产品场景，未公开本客户端需要的 RDP wire profile。因此本客户端不从 Azure 文档推导 HEVC 编号或封装，仍等待精确产品协议/授权抓包和 live 首帧、刷新、恢复证据。

## 2026-09-08：提交 `8e4215b` 的本地验证阶梯

本提交把 YUV420/YUV444 带 stride 平面的最小存储长度计算改为 checked arithmetic。
当远端声明的高度、stride 和行宽会使 `usize` 计算溢出时，转换边界返回
`InvalidPlane`，不会进入 SIMD 或逐像素读取；height 为零时仍保留“非零行宽必须有一行
存储”的既有校验契约。该修复只改变无效输入的 fail-closed 行为，不改变有效帧的像素
转换或 Bitmap/RemoteFX 回退。

在固定 Rust 1.96.0 的 macOS arm64 工作区执行了无凭据验证阶梯：

| 门禁 | 结果 |
|---|---|
| 格式与差异空白 | `cargo fmt --all -- --check`、`git diff --check` 通过 |
| RDP 协议测试 | `cargo test --locked -p frd-protocol-rdp --quiet`：181 passed、0 failed |
| workspace 测试 | `cargo test --locked --workspace --quiet`：508 passed、9 ignored、0 failed |
| pinned EGFX 与目标检查 | pinned `ironrdp-egfx` 19 项、8 个 Windows/Linux/macOS core/video/plugin target check 通过 |
| native FFmpeg fixture | macOS arm64 bundle 的 HEVC/AVC420/AVC444 fixture 5/5 通过，package verifier 通过 |

该阶梯没有读取凭据，也没有发起新的 RDP live 连接；真实 EGFX `CapabilitiesConfirm`、
AVC420/AVC444 首帧、持续刷新、恢复和 HEVC wire profile 门禁继续保持关闭。上述结果
证明本次平面长度修复与现有协议、解码器和 legacy 回退回归兼容，不把本地检查表述为
服务器互操作证据。

2026-09-07 的离线 H.264 门禁覆盖精确 `H264Avc420/Yuv420P8` 能力匹配、四字节
AVC length-prefixed NAL 到 Annex-B 的一次性转换、YUV420 三平面尺寸校验、FFmpeg
H.264 decoder/parser 构建选项，以及 Linux x86/i686、x86_64/arm64 bundle 脚本。FFmpeg 插件
ABI 的 codec capability 槽为零时会拒绝旧插件；native C bridge 在本机 FFmpeg 头文件
下完成语法检查和 `native-ffmpeg` cargo check。EGFX handler 已通过离线测试把已映射
AVC420 RGBA 更新转换为 generation-bound `SurfaceUpdate` 队列；协议中立 YUV420 decoder
到 IronRDP RGBA 的适配契约也已加入严格的能力、长度前缀、generation 和单帧门禁；active
session 会消费 EGFX Reset/Damage/FrameBoundary 并提交给 runtime。macOS arm64 已通过
native HEVC、AVC420、AVC444 fixture，但授权 Windows 目标尚未产生可审计的 EGFX surface
序列；Windows x86/ARM64 的 package/runtime gate 仍明确为 unavailable。

现代图形的实现顺序固定为 AVC420、RemoteFX 优化、AVC444、HEVC。HEVC 即使先有
本地解码能力，也必须等到真实生产互操作门禁通过并被服务端实际协商后，才可以成为
生产选择器的首选；在此之前继续使用传统回退路径。

## 2026-09-07：首次自动记录证书候选通过有界互操作

用户指定首次自动信任并保存、指纹变化停止自动连接后，在 `9c9f6db` 加本次工作树
变更的候选上复测。依赖仍为 IronRDP 0.17.0，客户端身份为 Macintosh。
现行规则见 [首次证书设计](../superpowers/specs/2026-09-07-rdp-first-use-certificate-design.md)。
用户凭据经非回显终端父进程和匿名 stdin 管道提供，未进入 argv、文件或日志。
探针专用指纹位于本地忽略的 `target/rdp-tofu-live-pins`，只保存证书 SHA-256，
不替代 Windows 当前用户 DPAPI 存储，也不声明 macOS GUI 已实现平台凭据服务。

| 真机步骤 | 实际结果 |
|---|---|
| 第一次连接 | `Unknown`；自动保存指纹后进入 `TransportReady` 与 activation，1280×720；57 个 frame boundary、1 个 FullBaseline、57 个 patch、20,197,376 decoded bytes |
| 第一次清理 | `exit=closed cleanup=joined`，进程退出码 0。会话在计划观察上限前结束；本探针没有记录对端正常关闭的进一步原因，不把它计作完整 20 秒观察 |
| 第二次连接 | 新进程读取同一指纹记录，`PinMatched`；自动继续认证与 activation，1280×720；133 个 frame boundary、1 个 FullBaseline、133 个 patch、41,082,880 decoded bytes |
| 第二次清理 | 首帧后约 20 秒发送 `Disconnect`，`exit=closed cleanup=joined`，进程退出码 0 |
| 指纹变化 | 确定性协议/应用/存储测试证明拒绝且不覆盖旧 pin；未修改真实服务器证书 |

frame boundary 数是 decoder 发布次数，不等同于显示帧率。首个 boundary 为
Incremental，此后覆盖累积到 1 个 FullBaseline；没有把首个局部更新误记为完整首帧。
该结果验证协议认证和像素解码，不证明 GUI 呈现、颜色观感、输入、音频、剪贴板、
动态分辨率、长期稳定性或 Windows 客户端包。

离线集成命令：

```text
cargo test --locked -p frd-protocol-rdp -p frd-app -p frd-platform-windows -p frd-shell-desktop -p frd-ui-egui
cargo build --locked -p frd-protocol-rdp --example bounded_live_probe
cargo fmt --all -- --check
git diff --check
```

Rust 1.96.0 下 RDP 115、应用 80、平台 26、桌面 shell 204、egui 34 项通过，
合计 459 项、0 failed。平台测试覆盖跨平台文件发布逻辑，Windows DPAPI 条件编译
测试未在 macOS 运行。标题栏详情沿用已测试的键盘聚焦 tooltip 与无障碍路径；
本轮没有 Windows 打包 GUI 的缩放/主题视觉复验，不能宣称完整 GUI 验收。

额外执行根包 `cargo test --locked`，在 macOS arm64 链接失败：未改动的
`src/arp.rs` 引用 Windows `SendARP` 符号。本次没有修改该既有平台限制，
不把根包全量测试报告为通过；上述 459 项相关 crate 测试独立通过。

## 2026-09-08：提交 `cec575f` 的 legacy-only 有界回归

在提交 `cec575f` 上再次使用同一个非回显 stdin 探针完成一次独立授权 Windows
目标的 legacy-only 回归。探针构造 `RdpProtocolFactory::new`，没有注册 EGFX
decoder provider，因此这次运行只验证当前 Bitmap/RemoteFX fallback 和会话生命周期，
不宣称服务器或客户端的 AVC420/AVC444/HEVC 能力。

| 真机步骤 | 实际结果 |
|---|---|
| 身份与认证 | `Unknown` 证书身份挑战自动保存/匹配后继续；进入 `TransportReady`，TLS/CredSSP/NLA 与 activation 成功 |
| 画面 | 1280×720；首个完整 baseline 已解码，后续继续收到增量更新 |
| 有界观察 | 20 秒内 `169` 个 frame boundary、`2` 个 FullBaseline、`169` 个 patch、`38,027,264` decoded bytes |
| 清理 | 发送 `Disconnect` 后 `exit=closed cleanup=joined`，进程正常回收 |
| 能力边界 | 未注册 EGFX provider；本次没有 RDPGFX `CapabilitiesConfirm`、AVC420/AVC444 或 HEVC wire 证据 |

该结果确认本提交没有破坏传统 RDP 画面回退路径，也不能替代 EGFX 首帧、持续刷新、
decoder 失败降级或恢复门禁。探针仍只输出非敏感阶段、指纹摘要和帧统计，凭据不进入
argv、文件或日志。

## 2026-09-07 历史尝试：独立目标可达，旧策略在 TLS 阶段阻塞

实现基线为 `9c9f6db`，客户端执行环境为 macOS arm64，Rust 1.96.0。
本轮用户已提供独立 Windows 目标及测试授权；目标、账号、密码和证书内容不写入
本记录。历史“没有独立授权目标”的前提已不适用于此次 macOS 探针，但 Windows
产品 GUI 的真机门禁仍未完成。

| 验证项 | 实际结果 |
|---|---|
| 网络 | 目标 TCP 3389 连接成功 |
| 独立无凭据协议观察 | RDP negotiation 选择 HYBRID（值 2，即 NLA/CredSSP）；TLS 1.3 握手成功。该观察连接未发送认证或桌面会话数据 |
| 证书观察 | 自签名叶证书，只有 CN，未包含 Subject Alternative Name；有效期为 2026-08-27 至 2027-02-26。按 IP 连接缺少可匹配的 IP SAN |
| 实际产品 adapter | `RdpProtocolFactory::new(RdpClientPlatformIdentity::Macintosh)` 经现有 runtime 执行，输出 `Connecting` 后返回 `rdp_tls_failed`；未发布可交互身份挑战 |
| 首帧和清理 | `frames=0 full_baselines=0 patches=0 decoded_bytes=0 cleanup=joined`，探针退出码 1 |
| 认证、画面、输入 | 未进入凭据认证；密码正确性、activation、首帧、刷新、键鼠均未验证 |

该次尝试的旧实现仅允许“未知签发者且主机名、有效期、用途检查通过”的证书显式确认。
观察到的无 IP SAN 证书与这一规则不兼容；adapter 只暴露统一 `rdp_tls_failed`，
没有进一步公开 macOS 信任库的底层错误，因此不把观察推断表述为底层错误日志。
本轮未绕过 TLS 策略、安装证书、修改服务器设置或实现其他认证路径。

新增 `crates/frd-protocol-rdp/examples/bounded_live_probe.rs` 作为可复用探针，
只经匿名 stdin 管道读取主机、用户名和密码；终端父进程使用非回显输入，凭据不进入
argv、文件或日志。若收到允许确认的身份挑战，必须显式输入匹配本次摘要的
`trust-once <SHA256>`，否则拒绝。此次未出现挑战，未执行信任决定。
探针最多观察 60 秒，首帧后最多 20 秒，随后断开并最多等待 15 秒清理；
不发送输入或剪贴板写入，不播放音频，不证明 GUI 渲染。

已运行 `cargo test --locked -p frd-protocol-rdp`：113 passed、0 failed、0 ignored，
另有 0 项文档测试；探针构建、`cargo fmt --all -- --check` 与 `git diff --check`
均通过。探针构建命令为
`cargo build --locked -p frd-protocol-rdp --example bounded_live_probe`。
初次工具链调用错误地使用
Homebrew rustc 1.87，因依赖 MSRV 被拒绝；显式前置项目 Rust 1.96.0 工具链路径后
完成构建与测试，未修改依赖锁文件。

该旧候选的结论为 **TLS 身份阶段阻塞**，不构成 IronRDP 远程 Windows 成功验证。其后继续完整
门禁需要目标提供与连接地址匹配、满足当前验证规则的证书，或另行设计审查兼容
Windows 原生自签名证书的明确身份信任方案；不能把本次失败解释为账号密码错误。

## 2026-09-05 平台身份正交化与交付门禁

本轮在分支 `codex/rdp-platform-orthogonality`、实现基线
`44d932bd6b000c85210fe0a40612525fd0095aee` 上执行。RDP 依赖保持为
`ironrdp` 0.17.0 与 `ironrdp-blocking` 0.10.0；实际 Rust 工具链为 rustc/cargo
1.96.0（`x86_64-pc-windows-msvc`，LLVM 22.1.2）。

### 显式客户端平台身份证据

- `frd-protocol-rdp` 公开闭集枚举 `RdpClientPlatformIdentity`，并把它作为
  `RdpProtocolFactory::new` 的必填参数保存到每次 `RdpConnectionConfig`；不存在
  `Default` 或从登录/profile 数据推导身份的路径。
- Windows composition root 仅在
  `apps/freeremotedesk-windows/src/main.rs` 显式构造
  `RdpProtocolFactory::new(RdpClientPlatformIdentity::Windows)`。
- adapter 的私有 upstream seam 把五个产品身份逐一映射为 IronRDP
  `MajorPlatformType`；`client_platform_identities_are_explicit_protocol_values` 与
  `approved_identities_map_to_exact_ironrdp_major_platform_types` 覆盖闭集和精确映射。
  因此协议身份由客户端 shell composition 注入，不再由编译当前 crate 的宿主
  `cfg(target_os)` 隐式决定。

### 本地命令与实际结果

下列命令均在
`D:\FreeRemoteDesk\.worktrees\mac-baseline-rdp-integration` 执行并退出成功：

```powershell
cargo fmt --all -- --check
cargo test --locked -p frd-protocol-rdp
cargo test --locked -p frd-shell-desktop
cargo test --locked -p freeremotedesk-windows --test dependency_boundary
cargo test --locked -p frd-ui-model -p frd-app
cargo check --locked --workspace --all-targets
cargo test --locked
cargo tree -p frd-protocol-rdp -e normal
rg -n "NoCertificateVerification|danger_accept_invalid_certs|SSLKEYLOGFILE|ClearTextPassword|--password" crates/frd-protocol-rdp apps/freeremotedesk-windows
git diff --check
cargo build --locked --release -p freeremotedesk-windows
pwsh -NoProfile -File tools/stage-windows-package.ps1 -PackageRoot target/package-rdp-orthogonality
pwsh -NoProfile -File tools/verify-windows-package.ps1 -PackageRoot target/package-rdp-orthogonality
```

另按托管 Windows workflow 的前置方式把同一 Release staged 到
`target/package-test`，再由 Windows PowerShell 5.1 导入 Pester 3.4.0 并执行：

```powershell
Invoke-Pester -Script tools/tests/windows-package.Tests.ps1 -EnableExit
```

| 门禁 | 2026-09-05 实际结果 |
|---|---|
| 格式 | 通过 |
| `frd-protocol-rdp` | 114 passed，0 failed，0 ignored |
| `frd-shell-desktop` | 214 passed，0 failed，0 ignored |
| Windows dependency boundary | 2 passed，0 failed，0 ignored |
| `frd-app` / `frd-ui-model` | 75 / 12 passed，0 failed，0 ignored |
| 根包 `cargo test --locked` | 166 passed，0 failed，2 ignored；两项均保留显式外部授权 capture fixture 原因 |
| workspace all-target check | 通过；仅根 legacy MVS capture binary 的 5 个既有 `dead_code` 警告 |
| 依赖树 | 确认 IronRDP 0.17.0；RDP 可选组件为 `cliprdr` 0.7.0、`displaycontrol` 0.8.0、`rdpsnd` 0.9.0 |
| 敏感模式扫描 | 仅命中受控的字面 `--password` 拒绝测试及 `--password-provider` 选项名；无明文密码路径、宽松证书验证器或 key-log 开关 |
| Windows PowerShell 5.1 / Pester 3.4.0 | 29 passed，0 failed，0 skipped/pending/inconclusive |
| 差异空白检查 | 通过 |

### 当前图形范围与 Windows 包

当前默认启用的传统图形基线仍为 Raw Bitmap、Interleaved RLE、RDP 6 Bitmap
compression 与 RemoteFX，统一发布 BGRX dirty rectangles。EGFX DVC 接缝、H.264
AVC420 FFmpeg bridge、provider 注入和 active-session 到 runtime 的 SurfaceUpdate 提交
边界已建立，并通过 pinned IronRDP PDU 和 native fixture 离线验证；固定 bundle 缺失时
仍回退 legacy。真实 RDPGFX/EGFX、ZGFX、AVC/AVC420、AVC444 互操作尚未完成。AVC444
目前具有规范结构 envelope、V1/V2 双流重建和色彩转换的构造期边界，但生产组合根和
服务器确认仍关闭；HEVC 仍只有协议无关 decoder/selector 门禁，不能作出现代图形已生产
支持的声明。

`cargo build --locked --release -p freeremotedesk-windows` 产生
42,824,704-byte 的 `target/release/freeremotedesk-windows.exe`，SHA-256 为
`BE298D369BF19B8A528FF71A6E931E2C3DADFA44BB02217C89D3BEED2C1AEB0D`。
stager 与独立 verifier 均通过。`target/package-rdp-orthogonality` 的完整文件集合
严格为一个 executable、三个固定 FFmpeg codec DLL、`ffmpeg-manifest.json`、
`FFmpeg-LGPL-2.1-or-later.txt` 与 `FFmpeg-NOTICE.txt`；Pester 29 项同时覆盖 exact
package、hash/provenance、DLL shadow、对应源码 staging 与安装/提权边界。该包未签名，
也不是 Windows RDP 真机互操作证明。

### Live gate：`BLOCKED_LIVE`

本轮没有与当前 Codex 主机分离、经用户授权的原生 Windows Remote Desktop Services
目标，也没有可用的独立本地 guest。唯一可推定的 localhost 路径可能锁定或切换正在
运行 Codex 的 active console，因此按安全 ruling 未发起 localhost RDP，也未读取或
传递任何目标凭据。证书确认、NLA、activation、`FullBaseline`、增量刷新、指针、键盘、
双轴滚轮、失焦 `ReleaseAll`、显式断开、返回登录页和 known-pin 重连均保持未验证。
状态为 `BLOCKED_LIVE`，不是测试通过或真机失败；README 的 Windows RDP 状态继续为
**开发中**。解除阻塞需要用户提供与当前主机隔离的授权 Windows 目标，并通过现有 GUI
和安全凭据存储执行有界门禁。

## Mac 基线集成刷新（2026-08-29）

RDP adapter 已移植到经 Mac 真机验证的 Windows winit/wgpu 产品基线，并仅在
Windows 应用 composition root 与 Apple adapter 并列注册。最终候选 `35e5962` 的
fresh 离线门禁结果如下：

- 两套完整 workspace 测试均为 868 passed、0 failed、11 个有理由的本地 fixture
  ignored；
- no-default workspace build、Windows Release build 和完整计划 `-D warnings`
  Clippy 均通过；
- `frd-protocol-rdp` 依赖/导入审计对 Apple、RFB、desktop shell、platform shell、
  winit、wgpu 和 egui 均为零命中；
- 最终重建的 Windows executable 为 42,106,880 bytes，SHA-256
  `F0A80A17150BD9E457DFBBDABD8B4070C294A98DCA0A0A215B44F646EB5B1A4B`。

同一候选完成了 macOS 自动选择、认证、首帧、MVS 增量、键鼠与正常断开回归；
这只证明 RDP 注册没有取代 Apple composition path，不构成任何 Windows RDP 真机
互操作。独立授权的 stock Windows 目标仍缺失，状态继续为 `BLOCKED_LIVE` / **开发中**。

已实现并纳入离线门禁的适配器边界包括：系统信任链和仅不受信任签发者可用的显式
证书确认/精确 pin、仅 CredSSP/NLA 的连接路径、licensing/activation 基线、BGRX
脏矩形发布、fast-path 键鼠、单主显示器 Display Control、adapter 内 Unicode 文本
CLIPRDR，以及 48 kHz
双声道 PCM RDPSND 发布。没有 decoder provider 时，离线测试断言 pinned IronRDP 的 outgoing capability 列表仅含
Phase-1 RemoteFX codec；传统图形基线为 Raw、Interleaved RLE、RDP 6 Bitmap 和
RemoteFX。
EGFX、ZGFX、AVC/AVC420 与 AVC444 均未完成生产互操作，不得因本次构建而作出支持声明。
当前 AVC444 已有规范结构 envelope、V1/V2 双流重建和色彩转换的构造期边界，但生产
组合根和服务器确认仍未完成。
未纳入当前范围的其他能力仍为：RDPDR/文件/磁盘/设备、AUDIN/客户端麦克风、网关、
智能卡、打印机和多显示器。

当前协议中立 `Modifiers` 没有 Caps Lock、Num Lock 或 Scroll Lock 状态位；锁定状态
同步不属于本分支验收，也没有新增公共输入/UI schema。物理修饰键及现有输入保持不变。
CLIPRDR 仍是 adapter-local、按协商能力作产品门控的离线实现；Windows 平台剪贴板
gate 未启用，本记录不构成端到端剪贴板证明。

RDPSND 的 `wFormatNo` 按客户端公布的共同格式列表解释，而不是按服务端原始 offer
位置解释。真实 PDU 回归覆盖了服务端位置 0 为不精确格式、位置 1 为精确 PCM 的
offer：客户端列表索引 0 的 Wave2 发布 PCM 帧，原服务端位置 1 的 Wave2 只在 adapter
内降级音频且不发布帧。这仍是离线协议证据，不是 Windows 真机音频互操作证明。

## 工具链

所有 Rust 命令均使用显式 `+stable`。已验证的实际工具链为：

- `rustc +stable -Vv`: rustc 1.96.0 (ac68faa20 2026-05-25)，
  `x86_64-pc-windows-msvc`，LLVM 22.1.2。
- `cargo +stable -V`: cargo 1.96.0 (30a34c682 2026-05-25)。

未使用名为 `1.96.0` 的不完整目录，也未安装或卸载任何工具链。

## 命令与结果

下列命令在 `D:\FreeRemoteDesk\.worktrees\windows-rdp` 执行，均退出成功，除明确
列出的 11 项既有、带原因的忽略测试外没有测试失败；“均退出成功”不包括下表单列的
严格 `-D warnings` Clippy 既有阻塞：

```powershell
cargo +stable fmt -- --check
cargo +stable test -p frd-protocol-rdp audio::tests -- --nocapture
cargo +stable test -p frd-protocol-rdp
cargo +stable test -p frd-shell-desktop
cargo +stable test -p freeremotedesk-windows --test dependency_boundary
cargo +stable test --workspace
cargo +stable test --workspace --no-default-features
cargo +stable test --workspace -- --list
cargo +stable test --workspace --no-default-features -- --list
cargo +stable check --workspace
cargo +stable check --workspace --no-default-features
cargo +stable clippy --workspace --all-targets
cargo +stable clippy --workspace --all-targets -- -D warnings
cargo +stable clippy -p frd-protocol-rdp --all-targets --no-deps -- -D warnings -A clippy::result_unit_err
cargo +stable build -p freeremotedesk-windows --release
cargo +stable build --workspace
cargo +stable build --no-default-features
cargo +stable tree -p frd-protocol-rdp -e normal
cargo +stable run -- --help
cargo +stable run -- hpssview --help
rg -n "NoCertificateVerification|danger_accept_invalid_certs|SSLKEYLOGFILE|--password|ClearTextPassword" crates/frd-protocol-rdp apps/freeremotedesk-windows
git diff --check
```

| 门禁 | 结果 |
|---|---|
| 格式 | `cargo +stable fmt -- --check` 通过 |
| RDP 音频聚焦测试 | 9 通过，0 失败，0 忽略；含 `wFormatNo` 正反真实 PDU 回归 |
| RDP 协议单元/文档测试 | 103 通过，0 失败，0 忽略 |
| 桌面 shell 单元/文档测试 | 40 通过，0 失败，0 忽略 |
| Windows 依赖边界集成测试 | 2 通过，0 失败，0 忽略 |
| 完整 workspace（默认特性） | 843 项列出；832 通过，0 失败，11 既有忽略 |
| 完整 workspace（`--no-default-features`） | 843 项列出；832 通过，0 失败，11 既有忽略 |
| 两个 workspace `cargo check` | 均通过；根 legacy binary 仅有下述 5 个既有 `dead_code` 警告 |
| 普通 workspace Clippy | 通过；报告既有 lint 警告，不含本轮新增 lint |
| 严格 workspace Clippy | 在未改动的 `frd-frame::PixelBuffer::len` 上因既有 `len_without_is_empty` 失败；`frd-protocol-rdp --no-deps` 继续暴露未改动 `config.rs` 的既有 `result_unit_err`。只豁免后者时，本轮 RDP 全目标以 `-D warnings` 通过 |
| 发布构建 | `freeremotedesk-windows` 通过 |
| 无默认特性构建 | 通过；见下方既有警告 |
| 顶层与 `hpssview` 帮助 | 均成功输出；既有 Apple/HPSS CLI 未改变 |
| 差异空白检查 | `git diff --check` 通过 |

通过 `cargo +stable test --workspace [--no-default-features] -- --list` 复核两个配置均列出
843 项；源代码中的 11 个 `#[ignore]` 均为现有、需要未纳入公开仓库的授权媒体/捕获
fixture 的测试。因此实际执行的 832 项全部通过。

`cargo +stable build --no-default-features` 发出 5 个既有 `dead_code` 警告，均位于旧
`src/vnc/mvs_capture_v2*.rs` 的历史/诊断捕获 API；该命令仍成功完成。它们不在 RDP
adapter 或 Windows 产品包中，且本离线门禁没有证明需要修改它们，因此未为消除警告
改变实现。

## 依赖与敏感数据边界

`cargo +stable tree -p frd-protocol-rdp -e normal` 确认的直接 RDP 关键依赖版本为：

- `ironrdp` 0.17.0、`ironrdp-blocking` 0.10.0；启用的受限服务组件为
  `ironrdp-cliprdr` 0.7.0、`ironrdp-displaycontrol` 0.8.0 和 `ironrdp-rdpsnd` 0.9.0。
- `rustls` 0.23.43、`rustls-platform-verifier` 0.7.0、`sha2` 0.10.9、`tokio` 1.53.1、
  `zeroize` 1.9.0。

敏感模式扫描没有发现 permissive certificate verifier、`danger_accept_invalid_certs`、
`SSLKEYLOGFILE` 或 `ClearTextPassword`。`--password` 的两个文本命中都是受控 CLI
测试/`--password-provider` 参数名称：前者验证字面密码参数被拒绝，后者选择非明文
凭据提供者；二者都不接受、记录或传递密码值。

## 发布产物

发布命令产生的 Windows 可执行文件：

- 路径：`D:\FreeRemoteDesk\.worktrees\windows-rdp\target\release\freeremotedesk-windows.exe`
- 大小：41,598,976 bytes
- SHA-256：`DAA75F0A3520F50BEB82898AC84EB6AC6622376F29EE21E33C705D74D2C2CD50`

哈希仅标识该工作树的本机构建产物；它不是签名、安装包验证，也不是任何在线
互操作证明。本记录不包含主机、凭据、证书 DER、会话密钥或捕获密钥材料。

## Task 10：受限 Windows 真机前提检查（2026-08-29，BLOCKED_LIVE）

本任务只执行了不会发起 RDP 协议、不会读取密码或证书材料的本机只读检查。没有
创建用户、修改服务/防火墙/策略/证书，也没有启动回环或局域网 RDP 登录。

| 检查 | 结果 |
|---|---|
| `Get-Service TermService` | `Running`，启动类型 `Manual`。 |
| `Get-NetTCPConnection -LocalPort 3389 -State Listen` | PID 29056 同时监听 `0.0.0.0:3389` 与 `[::]:3389`。 |
| `Get-CimInstance Win32_OperatingSystem` / `Win32_ComputerSystem` | 当前主机为 Windows 11 专业版 10.0.26200（工作组工作站）。 |
| `qwinsta` | 本地 `console` 会话 ID 1 为 `Active`（账号名已从记录删节）；仅有 `rdp-tcp` 的 `Listen` 条目。 |
| 产品安全 profile/pin 存储（只统计，不输出具体目标或账号） | `connections-v1.json` 有 1 个有效 profile，但 `windows-rdp` profile 为 0；server-identity-pins 目录不存在，pin 文件为 0。聚合脚本没有访问任何已保存 profile 的 `username` 字段，也没有枚举或读取 Windows Credential Manager。 |
| Release 产物 | `target\release\freeremotedesk-windows.exe` 存在（41,569,280 bytes）；其哈希见上节的 Task 9 离线构建记录。 |

上表 Release 行保留 Task 10 前提检查当时的历史产物；本轮最终 fix-wave 二进制已由
“发布产物”一节的新大小与 SHA-256 取代。

在本轮用户提供输入和已检查的产品配置范围内，唯一已知且可监听的 RDP 目标是运行
Codex 的当前主机。对该主机发起完整的回环 RDP 登录可能切换或锁定上述 Active
console，从而中断当前工作会话；已检查的产品 profile 中也没有另一台 RDP 目标。
因此未运行证书确认、NLA、首帧、
增量、输入、断开、pin 重连、能力协商或损伤路径测量。这是**缺少安全的已授权
目标**造成的 `BLOCKED_LIVE`，不是 adapter 的实现失败，也不能据此改变 Windows
RDP 的 **开发中** 状态。

要解除阻塞，需由用户提供一台与当前 Codex console 分离、已授权的原生 Windows
Remote Desktop Services 目标，并通过现有 GUI/安全凭据存储提供测试账号；不得通过
命令行传递密码。届时应在单次有界会话中依次验证未知证书确认、显式信任、NLA、
首帧、增量、颜色、指针、键盘、滚轮、失焦释放、断开和进程清理；已保存精确 pin
的重连以及证书不匹配 fail-closed 必须使用独立授权目标或确定性测试证书，绝不
修改线上服务器证书。只有登录、首帧、刷新、指针、键盘和断开全部通过，矩阵才可
升级为 **受限验证**。
