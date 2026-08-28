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
| Windows | winit + egui + wgpu | 键盘、鼠标 | Windows Release/ICO 资源开发中 | macOS；Windows RDP 开发中 | **开发中**；Windows-first 重构见 `docs/superpowers/specs/2026-08-27-winit-wgpu-windows-first-architecture-design.md`，Windows RDP 尚未接入产品 |
| macOS | 平台 shell 预留 | 计划中 | 计划中 | 尚无 | **计划中**；必须保留 macOS 原生标题栏、字体和 Keychain 适配 |
| Linux | 平台 shell 预留 | 计划中 | 计划中 | 尚无 | **计划中**；需实现窗口管理器适配与 Secret Service |
| Android | Rust 核心边界预留 | 触控/软键盘计划中 | 计划中 | 尚无 | **计划中**；桌面三平台完成后启动，需 Android Keystore 与自适应图标 |
| HarmonyOS NEXT 手机/PC | ArkUI/HUKS 边界设计 | 触控/键鼠计划中 | 计划中 | 尚无 | **计划中**；不是 Android 兼容层，须单独完成 ArkUI、HUKS 和构建 POC |

### 原生服务端目标

| 服务端系统 | 原生服务 | 客户端协议方向 | 当前客户端 | 总体状态 |
|---|---|---|---|---|
| macOS | Screen Sharing / Remote Management | Apple HPSS、RFB 线协议、MVS、Apple UDP 媒体 | Windows | **受限验证**；账号密码登录、画面、输入和 Mac→PC 音频已有有界真机证据 |
| Windows | Remote Desktop Services | 独立 `frd-protocol-rdp` + IronRDP 0.17.0 | Windows | **开发中**；私有 adapter 已实现服务器身份验证、TLS、CredSSP/NLA、licensing 与 activation，2026-08-29 证据仅限单元/workspace 测试，尚无 Windows 真机登录、首帧或输入互操作；不得要求安装 FreeRemoteDesk 服务端 |
| Linux | 系统或发行版原生 VNC/RFB 服务 | RFB 3.x 及服务端公开扩展 | 尚无 | **计划中**；不得引入配套守护进程 |

### Windows 客户端连接 macOS 功能明细

| 功能 | 协议/模块 | 状态 | 验证范围或阻塞点 |
|---|---|---|---|
| Mac 账号密码认证 | Apple HPSS 会话 | **已验证** | 使用 Mac 本地用户名/密码；不请求、保存或使用 Apple ID 凭据 |
| 完整桌面画面 | Apple HPSS + MVS type-0 | **已验证** | 真机可显示并完成键鼠交互；现行 GUI 重构仍需持续回归 |
| 增量桌面更新 | ARD 3.10 MVS type-1 | **已验证** | 严格回放 18 条记录并完成有界真机更新；见 `AGENTS.md` P2 |
| 鼠标输入 | Apple 会话输入消息 | **已验证** | 仅窗口与远程内容具备所需焦点时发送；移出窗口不继续注入 |
| 键盘输入 | Apple 会话输入消息 | **已验证** | 基础按键与修饰键已真机验证；平台 IME 完整适配仍需单列验证 |
| 动态分辨率 | 实验性 resized `0x09` + generation 切换 | **实验性** | 默认关闭；尚无足够 Apple 线协议互操作证据，见 `AGENTS.md` P1 |
| Mac→Windows 音频 | Apple UDP/SRTP + AAC-ELD | **受限验证** | 已认证、解码非静音 48 kHz 双声道并通过 Windows 输出；未宣称任意丢包与长时间 rollover |
| UDP 媒体传输 | Apple Message 1/2、`0x1c`、SRTP/SRTCP | **受限验证** | 音频和视频 socket 已完成有界真机互操作；长时间网络稳定性未覆盖 |
| Windows→Mac 麦克风 | Apple Audio Chat / IDS 路径 | **不支持** | 原生用户名密码 HPSS 会话没有已恢复的 Audio Chat 分支；Apple ID 与服务端助手均超出产品边界 |
| 剪贴板 | 能力边界已预留 | **计划中** | 当前 Windows 产品未完成端到端剪贴板集成 |
| 动态保存登录信息 | Windows Credential Manager + 非敏感配置 | **开发中** | 自动化状态机、非敏感元数据及本机进程唯一凭据库往返已通过；按本矩阵定义，授权 Mac GUI 的 TransportReady 提交与取消保存删除链路尚未完成有界真机验证，见 `docs/validation/windows-secure-login.md` |
| 文件传输 | 未选择 | **计划中** | 需先确认各原生服务端支持的协议与安全边界 |

### Windows 客户端连接 Windows 功能明细

| 功能 | 协议/模块 | 状态 | 验证范围或阻塞点 |
|---|---|---|---|
| 服务器身份与 TLS | RDP TLS + 系统信任链 + SHA-256 pin | **开发中** | 已实现系统信任链、主机名、有效期、未知证书显式确认、精确 pin 重连和指纹变化 fail-closed；身份确认及第二次 TLS 验证前不读取用户名/密码。2026-08-29 证据仅限 `frd-protocol-rdp` 单元测试与 workspace 测试，尚无 Windows 真机证书互操作证明 |
| 账号密码认证 | CredSSP/NLA | **开发中** | 私有 adapter 已实现只允许 NLA/TLS 的 CredSSP、licensing 与 activation 基线；2026-08-29 证据仅限单元/workspace 测试，尚无 Windows 真机登录或会话证明。凭据不得进入 argv、普通配置、日志或抓包 |
| 基础桌面画面 | Raw、Interleaved RLE、RDP 6 Bitmap、RemoteFX | **开发中** | 设计为 BGRX 脏矩形发布；尚无产品构建或真机首帧证据 |
| 鼠标与键盘 | RDP fast-path input | **开发中** | 设计包含 scan code、Unicode、鼠标、滚轮和失焦 `ReleaseAll`；尚未完成产品互操作 |
| 动态分辨率与多显示器 | Display Control DVC | **开发中** | 已通过现有 viewport 接口实现单主显示器 latest-only 调整，仅在 DVC 打开且服务端能力就绪后宣告，并在精确 reactivation 尺寸确认后切换 generation；多显示器仍不支持。2026-08-29 证据仅限单元/workspace 测试，无 Windows 真机互操作证明 |
| 现代图形 | EGFX、ZGFX、AVC420 | **计划中** | 只允许作为 RDP adapter 内部解码路径发布现有 `SurfaceUpdate`，不新增 UI；不得宣告 IronRDP 0.17.0 尚未实现的 AVC444/AVC444v2 |
| 文本剪贴板 | CLIPRDR | **开发中** | 已复用现有剪贴板能力和事件接口适配 Unicode 文本，读写方向按实际协商分别宣告；文件能力保持关闭。2026-08-29 证据仅限单元/workspace 测试，无 Windows 真机互操作证明 |
| Windows→客户端音频 | RDPSND | **开发中** | 已将共同协商的 48 kHz 双声道 16-bit PCM 通过协议中立 `MediaFrame` 端口发布，媒体背压只降级音频，RDP adapter 不打开平台音频设备。2026-08-29 证据仅限单元/workspace 测试，无 Windows 真机互操作证明 |
| 客户端麦克风、文件、磁盘与设备 | RDPEAI、CLIPRDR 文件、RDPDR | **计划中** | 不在当前 RDP 开发范围，也不新增入口或公共接口；未来必须单独设计并获得批准 |

当前 RDP 开发只适配 FreeRemoteDesk 已有的统一登录、证书确认、画面、键鼠、
动态分辨率、文本剪贴板、远程音频、状态与断开接口。不得为了暴露 IronRDP 的
其他能力扩展当前 UI 或影响 Apple adapter；未纳入现有公共接口的能力保持隐藏。

### 客户端与服务端组合

| 客户端 \ 服务端 | macOS 原生服务 | Windows 原生服务 | Linux 原生服务 |
|---|---|---|---|
| Windows | **受限验证** | **开发中** | **计划中** |
| macOS | **计划中** | **计划中** | **计划中** |
| Linux | **计划中** | **计划中** | **计划中** |
| Android | **计划中** | **计划中** | **计划中** |
| HarmonyOS NEXT 手机/PC | **计划中** | **计划中** | **计划中** |

矩阵只记录已完成的实际层级：编译通过、安装包生成、客户端本地运行、协议
实现和真机互操作必须分别验证，不能相互替代。任何新增功能或平台改动都必须
在同一提交中更新本节。

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
