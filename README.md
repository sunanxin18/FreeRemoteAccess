# FreeRemoteDesk — ARP 发现 + VNC 连接 macOS

纯 Rust 实现的局域网远程桌面客户端（Windows），复刻 Remote Desktop Manager 连接 Mac mini 的关键链路：

1. **ARP 发现**：扫描局域网，找出在线设备并识别 Apple 设备；
2. **VNC(RFB) 连接**：与 macOS 内置"屏幕共享"（Screen Sharing，本质是 VNC 服务端，TCP 5900）完成协议握手、DES 密码认证，支持查看 / 截图 / 键鼠控制。

不依赖 Npcap、不需要管理员权限、不需要任何 VNC 第三方库——ARP 用 Windows 内核 `SendARP`，RFB 协议全部手写。

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
