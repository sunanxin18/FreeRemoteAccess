# FreeRemoteDesk

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
| Windows | winit + egui + wgpu | 键盘、鼠标 | Release binary staging、固定 FFmpeg DLL/manifest/LGPL/对应源码校验已完成；MSI/MSIX 仍开发中 | macOS；Windows RDP 开发中 | **开发中**；统一视频 decoder 的编译、离线 fixture、DX12 readback、package staging 与 codec present/absent 单实例 GUI 门禁已完成，Apple Standard/HP 与 RDP 的当前真机边界见 [`cross-platform-video-decoder-20260901.md`](docs/validation/cross-platform-video-decoder-20260901.md)；RDP 仍等待独立授权的原生 Windows 目标完成登录、首帧与输入门禁。 |
| macOS | 平台 shell 预留 | 计划中 | 计划中 | 尚无 | **计划中**；必须保留 macOS 原生标题栏、字体和 Keychain 适配 |
| Linux | 平台 shell 预留 | 计划中 | 计划中 | 尚无 | **计划中**；需实现窗口管理器适配与 Secret Service |
| Android | Rust 核心边界预留 | 触控/软键盘计划中 | 计划中 | 尚无 | **计划中**；桌面三平台完成后启动，需 Android Keystore 与自适应图标 |
| HarmonyOS NEXT 手机/PC | ArkUI/HUKS 边界设计 | 触控/键鼠计划中 | 计划中 | 尚无 | **计划中**；不是 Android 兼容层，须单独完成 ArkUI、HUKS 和构建 POC |

### 视频解码后端状态

| 客户端平台/路径 | 状态 | 验证范围或阻塞点 |
|---|---|---|
| Windows native capability probe | **受限验证** | 2026-09-01 在单台 AMD Radeon 780M Windows 主机完成 D3D12 profile 探针；Main/Main10 报告 hardware exact，Main444 明确不可用。证据为 [`windows-video-capabilities-20260901.json`](docs/validation/windows-video-capabilities-20260901.json)，仅证明能力探针，不证明 native decoder 或远端会话首帧；Task 10 复跑结果见 [`统一视频解码器验收记录`](docs/validation/cross-platform-video-decoder-20260901.md)。 |
| Windows FFmpeg 8.1.2 Main444 software backend | **受限验证** | 固定签名源码构建的 LGPL 动态插件通过离线 Main444 fixture 精确解码，DX12 YUV444 离屏颜色/crop 门禁与 Windows package staging/verifier 通过；codec present/absent 均不阻止 GUI 启动。此状态只覆盖离线 fixture、GPU readback 与 binary staging，不是 system-owned 安装器、可见 runtime backend 状态或 Apple HP 真机首帧证明，见 [`统一视频解码器验收记录`](docs/validation/cross-platform-video-decoder-20260901.md)。 |
| Apple High Performance 真机首帧 | **开发中** | RTP/AU 与 decoder pipeline 已实现，但只有已 admission 的 current-generation surface 精确 `FramePresented(FullBaseline)` 才能进入 Ready 并开放输入。本轮独立 HP 尝试未发起到认证，未证明 authenticated RTP → Main444 config/AU → FFmpeg → exact present → Ready；不回退 Standard/MVS，见 [`统一视频解码器验收记录`](docs/validation/cross-platform-video-decoder-20260901.md)。 |
| macOS / Linux native video backend | **计划中** | 尚无 native decoder、平台 shell 或 package 构建验证。 |
| Android native video backend | **计划中** | 尚无 MediaCodec bridge、移动端 shell 或 package 构建验证。 |
| HarmonyOS NEXT native video backend | **计划中** | 必须单独完成 ArkTS/ArkUI 与 native codec bridge POC；不是 Android 兼容层，当前不冒充 build 支持。 |

### 原生服务端目标

| 服务端系统 | 原生服务 | 客户端协议方向 | 当前客户端 | 总体状态 |
|---|---|---|---|---|
| macOS | Screen Sharing / Remote Management | `frd-protocol-apple`：严格 Apple High Performance HPSS/MVS | Windows | **开发中**；当前产品仅允许加密 High Performance 路径，设计见 [`Apple High Performance Session`](docs/superpowers/specs/2026-08-29-apple-high-performance-session-design.md)。既有账号密码、共享会话画面/输入及媒体证据早于严格虚拟显示确认门禁，不能证明当前产品模式已被 stock macOS 接受；离线实现允许 `display_count=2` 确认 High Performance 并激活 publisher，但动态分辨率只允许 `display_count=1` |
| Windows | Remote Desktop Services | 独立 `frd-protocol-rdp` + IronRDP 0.17.0 | Windows | **开发中**；私有 adapter 已实现服务器身份验证、TLS、CredSSP/NLA、licensing 与 activation，2026-08-29 证据仅限单元/workspace 测试，尚无 Windows 真机登录、首帧或输入互操作；完整离线门禁、构建哈希和未验证边界见 [`docs/validation/windows-native-rdp.md`](docs/validation/windows-native-rdp.md)；不得要求安装 FreeRemoteDesk 服务端 |
| Linux | 系统或发行版原生 VNC/RFB 服务 | RFB 3.x 及服务端公开扩展 | 尚无 | **计划中**；不得引入配套守护进程 |

### Windows 客户端连接 macOS 功能明细

本表中既有“已验证/受限验证”记录描述对应认证、MVS、输入或媒体子系统的历史
证据，不自动继承为当前严格 High Performance 产品组合的互操作结论。当前组合
必须另外证明 stock macOS 接受虚拟显示、实体显示器按预期置黑并可在断开后恢复；
完成该有界门禁前，组合状态保持 **开发中**。

| 功能 | 协议/模块 | 状态 | 验证范围或阻塞点 |
|---|---|---|---|
| Mac 账号密码认证 | Apple HPSS 会话 | **已验证** | 使用 Mac 本地用户名/密码；不请求、保存或使用 Apple ID 凭据 |
| High Performance 虚拟显示与实体显示器置黑 | `frd-protocol-apple` 严格确认门禁 | **开发中** | 产品只选择加密 `APPLE_SRP`，发送现有 `0x1d` 后等待严格 `0x451 ServerState`，确认前不公开 generation/readiness；`display_count=2` 的匹配状态仍可确认 High Performance 并激活 publisher，单显示器限制只属于动态分辨率 eligibility；自动化门禁不能替代实体显示器置黑/恢复与完整远程桌面的真机观察，见 [`设计`](docs/superpowers/specs/2026-08-29-apple-high-performance-session-design.md) |
| 完整桌面画面 | Apple HPSS + MVS type-0/type-1 | **已验证** | 2026-08-31 保留固定捕获仅证明采样候选 `c57dc77` 的 Windows wgpu frame-transaction 路径在有界 Apple HPSS/MVS 真机比较中通过；范围、run id 与二进制身份见 [`windows-apple-wgpu-parity.md`](docs/validation/windows-apple-wgpu-parity.md)。这不包含后置 runtime 正确性修复，也不包含严格 High Performance 虚拟显示/实体显示器置黑与恢复门禁。 |
| 增量桌面更新 | ARD 3.10 MVS type-1 | **已验证** | 严格回放 18 条记录并完成有界真机更新；type-1 原位更新持久 CPU surface，只发布 MVS dirty rect，mailbox 不克隆像素，wgpu 只上传对应矩形。`3e375c0` 还把超过 32 个稀疏 dirty rect 确定性分为最多 32 个局部 patch，禁止退化为近整屏的全局包围矩形。2026-08-31 的 `c57dc77` 采样候选通过固定真机比较；后置修复只有离线证据，范围与非结论见 [`windows-apple-wgpu-parity.md`](docs/validation/windows-apple-wgpu-parity.md)。 |
| 鼠标输入 | Apple 会话输入消息 | **已验证** | 仅窗口与远程内容具备所需焦点时发送；移出窗口不继续注入 |
| 键盘输入 | Apple 会话输入消息 | **已验证** | 基础按键与修饰键已真机验证；平台 IME 完整适配仍需单列验证 |
| 动态分辨率 | 实验性 resized `0x09` + generation 切换 | **实验性** | 默认关闭；离线 eligibility 只接受匹配初始 `ServerState` 的 `display_count=1`，`display_count=2` 仅关闭 dynamic、不会关闭已确认的 High Performance 或 publisher；尚无足够 Apple 线协议互操作证据，见 `AGENTS.md` P1 |
| Mac→Windows 音频 | Apple UDP/SRTP + AAC-ELD | **受限验证** | 已认证、解码非静音 48 kHz 双声道并通过 Windows 输出；未宣称任意丢包与长时间 rollover |
| UDP 媒体传输 | Apple Message 1/2、`0x1c`、SRTP/SRTCP | **受限验证** | 音频和视频 socket 已完成有界真机互操作；长时间网络稳定性未覆盖 |
| Windows→Mac 麦克风 | Apple Audio Chat / IDS 路径 | **不支持** | 原生用户名密码 HPSS 会话没有已恢复的 Audio Chat 分支；Apple ID 与服务端助手均超出产品边界 |
| 剪贴板 | 能力边界已预留 | **计划中** | 当前 Windows 产品未完成端到端剪贴板集成 |
| 动态保存登录信息 | Windows Credential Manager + 非敏感配置 | **开发中** | 自动化状态机、非敏感元数据及本机进程唯一凭据库往返已通过；按本矩阵定义，授权 Mac GUI 的 TransportReady 提交与取消保存删除链路尚未完成有界真机验证，见 `docs/validation/windows-secure-login.md` |
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
| 服务器身份与 TLS | RDP TLS + 系统信任链 + SHA-256 pin | **开发中** | 已实现系统信任链、主机名、有效期、server-auth/EKU、仅不受信任签发者可显式确认、精确 pin 重连和指纹变化 fail-closed；错误主机、过期/尚未生效、用途错误及畸形证书不可交互覆盖。身份确认及第二次 TLS 验证前不读取用户名/密码。2026-08-29 证据仅限 `frd-protocol-rdp` 单元测试与 workspace 测试，尚无 Windows 真机证书互操作证明 |
| 账号密码认证 | CredSSP/NLA | **开发中** | 私有 adapter 已实现只允许 NLA/TLS 的 CredSSP、licensing 与 activation 基线；2026-08-29 证据仅限单元/workspace 测试，尚无 Windows 真机登录或会话证明。凭据不得进入 argv、普通配置、日志或抓包 |
| 基础桌面画面 | Raw、Interleaved RLE、RDP 6 Bitmap、RemoteFX | **开发中** | 已有 `freeremotedesk-windows` Release 构建，设计为 BGRX 脏矩形发布；尚无 Windows 真机首帧证据 |
| 鼠标与键盘 | RDP fast-path input | **开发中** | 已有离线 scan code、物理修饰键、Unicode、鼠标、滚轮和失焦 `ReleaseAll` 覆盖；当前协议中立 `Modifiers` 没有 Caps/Num/Scroll Lock 状态位，锁定状态同步明确延期且本分支不新增公共输入/UI schema；尚未完成产品互操作 |
| 动态分辨率与多显示器 | Display Control DVC | **开发中** | 已通过现有 viewport 接口实现单主显示器 latest-only 调整，仅在 DVC 打开且服务端能力就绪后宣告，并在精确 reactivation 尺寸确认后切换 generation；多显示器仍不支持。2026-08-29 证据仅限单元/workspace 测试，无 Windows 真机互操作证明 |
| 现代图形 | EGFX、ZGFX、AVC/AVC420、AVC444 | **计划中** | 均未实现或验证，不得宣告支持；未来只允许作为 RDP adapter 内部解码路径发布现有 `SurfaceUpdate`，不新增 UI |
| 文本剪贴板 | CLIPRDR | **开发中** | CLIPRDR 仅在私有 RDP adapter 内适配 Unicode 文本，并由现有协商能力作产品门控；Windows 平台剪贴板 gate 本分支未启用，文件能力保持关闭，不能宣称端到端剪贴板。2026-08-29 证据仅限 adapter 单元/workspace 测试，无 Windows 真机互操作证明 |
| Windows→客户端音频 | RDPSND | **开发中** | 已将共同协商的 48 kHz 双声道 16-bit PCM 通过协议中立 `MediaFrame` 端口发布；`wFormatNo` 按客户端公布的共同格式列表索引验证，媒体背压只降级音频，RDP adapter 不打开平台音频设备。2026-08-29 证据仅限单元/workspace 测试，无 Windows 真机互操作证明 |
| 客户端麦克风、文件、磁盘与设备 | RDPEAI、CLIPRDR 文件、RDPDR | **计划中** | 不在当前 RDP 开发范围，也不新增入口或公共接口；未来必须单独设计并获得批准 |

当前 RDP 开发只适配 FreeRemoteDesk 已有的统一登录、证书确认、画面、键鼠、
动态分辨率、文本剪贴板、远程音频、状态与断开接口，以及实现这些接口所必需的
IronRDP 协议内部要求。IronRDP 的其他能力不属于当前 roadmap；不得为其扩展当前
UI、公共接口或平台服务，RDP adapter 的本地改动也不得改变或门控 Apple
HPSS/ARD/MVS 路径。

### 客户端与服务端组合

| 客户端 \ 服务端 | macOS 原生服务 | Windows 原生服务 | Linux 原生服务 |
|---|---|---|---|
| Windows | **开发中** | **开发中** | **计划中** |
| macOS | **计划中** | **计划中** | **计划中** |
| Linux | **计划中** | **计划中** | **计划中** |
| Android | **计划中** | **计划中** | **计划中** |
| HarmonyOS NEXT 手机/PC | **计划中** | **计划中** | **计划中** |

矩阵只记录已完成的实际层级：编译通过、安装包生成、客户端本地运行、协议
实现和真机互操作必须分别验证，不能相互替代。任何新增功能或平台改动都必须
在同一提交中更新本节。

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
