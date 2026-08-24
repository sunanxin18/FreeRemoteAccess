# Apple Remote Desktop（ARD）协议全量分析

> 本文档为 FreeRemoteDesk 项目的协议研究资料，分析对象是 macOS「屏幕共享 / Apple Remote Desktop」
> 在 TCP 5900 上使用的私有协议扩展（Apple 未公开规范，全部来自社区逆向与公开资料，已在文中逐条标注来源）。
> 写作时间：2026-08-18。文中「本机实测」指用本仓库 `info` 子命令对本局域网 Mac mini 的握手探测结果
> （见 `CREDENTIALS.local.md`，此处不引用具体地址）。

---

## 目录

1. [产品与协议族总览](#一产品与协议族总览)
2. [传输层与 RFB 握手](#二传输层与-rfb-握手)
3. [安全类型总表](#三安全类型总表)
4. [类型 30：ARD 认证字节级全流程](#四类型-30ard-认证字节级全流程)
5. [新一代认证（33/35/36 与 RSA-SRP）](#五新一代认证333536-与-rsa-srp)
6. [认证后的会话与 Apple 私有扩展](#六认证后的会话与-apple-私有扩展)
7. [3283 管理通道](#七3283-管理通道)
8. [安全性分析](#八安全性分析)
9. [在 FreeRemoteDesk 中实现类型 30 的路线图](#九在-freeremotedesk-中实现类型-30-的路线图)
10. [参考资料](#十参考资料)

---

## 一、产品与协议族总览

### 1.1 名词澄清

| 名称 | 是什么 |
|---|---|
| **Apple Remote Desktop (ARD)** | Apple 的商业远程管理软件（约 $80），管理员端功能最全（批量管理、报告、软件分发）。客户端组件自 macOS 10.5 起内置 |
| **屏幕共享 (Screen Sharing)** | macOS 内置的 ARD 精简客户端（`/System/Library/CoreServices/Screen Sharing.app`） |
| **screensharingd** | 服务端守护进程（`/System/Library/CoreServices/RemoteManagement/screensharingd.bundle`），监听 5900，以 root 运行 |
| **远程管理 (Remote Management)** | 系统设置里的开关，本质是「允许 ARD 管理端接入」，与「屏幕共享」共用同一 5900 服务端（两者互斥开启） |
| **ARD 协议** | 指上述服务端在标准 RFB(VNC) 之上叠加的私有扩展：**私有认证类型（30/33/35/36）+ 私有编码/伪编码 + 私有会话特性** |

「ARD 协议」没有官方规范。Apple 官方文档只给了高层描述（认证基于 Diffie-Hellman，凭据用 AES 加密），字节级细节全部来自社区逆向——nmap、gtk-vnc、Tenable、Remotix/Devolutions、barneygale、以及 2026 年 8 月安全研究者在 CVE 分析中的披露。

### 1.2 端口

| 端口 | 协议 | 用途 |
|---|---|---|
| **TCP 5900** | RFB (VNC) + ARD 扩展 | 屏幕观察/控制、键盘鼠标输入、文件拖放（本文重点） |
| TCP 3283 | ARD 管理 | ARD 管理端命令通道（开关机、锁屏 curtain、软件分发等） |
| UDP 3283 | ARD 报告 | 客户端向管理端回传的报表数据 |

（来源：Apple「Remote Desktop 3 Network Administrator Guide」的端口列表；Apple 官方支持文档[Encrypt network data in Remote Desktop](https://support.apple.com/guide/remote-desktop/encrypt-network-data-apdfe8e386b/mac)）

### 1.3 版本沿革（与协议相关的变化）

| 时间 | 事件 |
|---|---|
| 2002 | ARD 1.x（Mac OS X 10.1/10.2 时代），管理协议 + VNC |
| 2006–2011 | ARD 2.x/3.x：引入 DH+AES 认证（官方手册描述为「与个人文件共享类似的 DH，512-bit 素数」） |
| 2011 (10.7 Lion) | **VNC 认证(类型 2)登录后出现登录屏幕，需再输 Mac 账号密码；ARD 认证(30)则直接进桌面**——这是第三方客户端必须实现类型 30 的动机（cafbit） |
| 2011 (ARD 3.5) | Snow Leopard 上右键映射 quirk：button-2 当右键（见 §6.5） |
| ~2014 (10.10+) | 安全类型列表扩展为 `[30, 33, 36, 2?, 35]`，33/35/36 为新一代 RSA-SRP 系认证 |
| 2020 (10.15) | Guacamole 报告：选择 30 后服务器回吐类型列表，旧式 30 握手出现兼容性问题（GUACAMOLE-1133，见 §4.6） |
| 2026-08 | **CVE-2026-65400**：screensharingd 的 SRP 实现两个逻辑漏洞（陈旧返回值 + 状态机失步），pre-auth 远程 root，野外利用（见 §8.3） |

---

## 二、传输层与 RFB 握手

### 2.1 banner 指纹

macOS 屏幕共享服务端主动发送的版本 banner 是**非标准版本号**：

```
RFB 003.889\n
```

`3.889` 不是真实 RFB 版本。标准客户端应回 `RFB 003.008\n`（按 3.8 会话），macOS 接受；`003.889` 本身可当作「对端是 AppleVNC 服务端」的指纹（nmap 的 `vnc-info` 脚本就是这么做的，其 versions 表专门收录了 `["RFB 003.889"] = "3.889"`）。

> 本仓库 `src/vnc/client.rs::negotiate()` 已兼容：解析出非 3/7/8 的次版本号一律按 3.8 会话处理。

### 2.2 安全类型列表（实测）

```
本机实测（2026-08-17，Mac mini，已启用 VNC 密码）：[30, 33, 36, 2, 35]
Tenable 实抓（2017，macOS 10.13，未启用 VNC 密码）：[30, 33, 36, 35]
Apple 讨论帖（2010，10.6 时代）                   ：[30, 2, 35]
```

规律：`30` 恒在；`2` 仅在勾选「VNC 显示程序可以使用密码控制屏幕」后出现；`33/36/35` 是较新 macOS 增加的新一代认证。

---

## 三、安全类型总表

| 类型 | 名称 / 出处 | 状态 | 说明 |
|---|---|---|---|
| 1 | None | 标准 | macOS 不提供 |
| 2 | VNC Authentication（DES） | 标准 RFC 6143 §7.2.2 | **本仓库已实现**；仅前 8 字节密码有效；登录后（10.7+）会遇到登录屏幕 |
| **30** | **ARD 认证**。RealVNC 日志称 `Ard(30)`；nmap 称 `Apple Remote Desktop` | 私有，**已完整逆向**（§4） | DH-1024 + MD5 派生 + AES-128-ECB 加密 `username[64]+password[64]` 凭据块。凭据是 **Mac 真实本地账号** |
| 33 | Apple RSA-SRP 混合（帧头魔数 "RSA1"） | 私有，部分逆向（§5.0 末） | 10.10+ 出现；Apple 客户端默认优先选择它 |
| 35 | nmap 称 `Mac OS X security type` | 私有，未公开字节格式 | 2010 年已存在（当时与 30 并列），现代 macOS 仍列出 |
| **36** | **Apple SRP-6a**（本仓库逆向命名） | 私有，**已完整逆向并实现**（§5.0） | corecrypto SRP-6a（RFC 5054 4096 组 + SHA-512 + PBKDF2-128B 预哈希），凭据为 Mac 真实账号；`src/vnc/srp.rs` |

33/35/36 属于 Apple 新一代「原生认证」体系。**类型 36（SRP）已于 2026-08-18 完整逆向并在本仓库实现**（§5.0）。综合 2021 年 barneygale 的逆向与 2026 年 Huntress 对 CVE-2026-65400 的分析，该体系为 **RSA + SRP（Secure Remote Password）混合**（macOS Endpoint Security 事件里合法登录的 `authentication_type` 字段为 **`RSA-SRP`**），会话密钥为 128-bit AES，击键加密、图像明文（§5）。

---

## 四、类型 30：ARD 认证字节级全流程

> 本节是全文最可靠的部分：cafbit（David Simmons, 2011，经 gtk-vnc 的 Håkon Enger 补丁还原）、
> Stack Overflow 6938432、Tenable 实抓报文（2017）、nmap `nselib/vnc.lua::login_ard()` 四方交叉一致。

### 4.1 流程图

在标准 RFB 3.8 握手（版本交换 → 服务器发类型列表 `n, t1..tn`）之后，客户端发送 `[30]` 选择该类型，随后：

```
客户端                                        服务器 (macOS:5900)
   | >> [30]                                    选择安全类型
   |                                            （服务端不回 ACK，直接下发 DH 参数）
   | << u16 generator          ┐
   | << u16 keyLength          │ 服务器 DH 材料
   | << byte[keyLength] modulus│
   | << byte[keyLength] srvPub ┘
   |                                            （客户端本地计算）
   | << byte[128] ciphertext    ┐ 凭据块 AES-128-ECB( MD5(shared) )
   | << byte[keyLength] cliPub  ┘ 客户端 DH 公钥
   | << u32 result              0 = 成功，1 = 失败（失败时按 3.8 跟 u32 长度+原因？实测仅 u32）
   | >> ClientInit(1)                          成功后进入标准 RFB 会话
   | << ServerInit(...)
```

### 4.2 服务器 DH 材料字段表（易错点！）

| 偏移 | 长度 | 字段 | 说明 |
|---|---|---|---|
| 0 | 2 | `generator` | **是大端 u16 的值本身**（实测恒为 `0x0002`），**不是**「长度前缀 + keyLength 字节大数」 |
| 2 | 2 | `keyLength` | 后续模数/公钥的字节长度，大端 u16。实测 `128`（1024-bit DH） |
| 4 | keyLength | `modulus` | 素数 p，大端 |
| 4+keyLength | keyLength | `serverPubKey` | 服务器公钥 A = g^a mod p，大端，**无长度前缀** |

服务器消息总长 = `4 + 2*keyLength` = 260 字节（keyLength=128 时）。

> ⚠️ 常见误记：把这个格式记成「u16 len + 256 字节模数、u16 len + 256 字节生成元、256 字节公钥」。
> 正确顺序是 **g(2B 值) → keyLen(2B) → N → A**，且生成元只是一个 16 位整数。
> （Tenable 原文："The generator value is always two bytes and is first in the packet.
> The key length is next and is a two byte integer. The prime modulus and public key follow
> and are the same size as the key length."）

### 4.3 实测参数

**macOS 26.6.1（2026-08，RDM 会话经本仓库 proxy 抓包 + RFC 5054 逐字节比对确认）：**

```
Generator     : 0005  （g=5）
Key Length    : 512 字节（4096-bit DH）
Prime Modulus : 与 RFC 5054 Appendix A 的 4096-bit 组模数【完全一致】
                （即 corecrypto `ccsrp_gp_rfc5054_4096` 常量，TLS-SRP 4096 组）
Server PubKey : 每连接随机 512 字节
服务器材料总长: 4 + 2×512 = 1028 字节；客户端响应 = 128B 密文 + 512B 公钥 = 640 字节
```

**macOS 10.13（2017，Tenable 抓包，旧参数）：**

```
Generator     : 0002  （g=2）
Key Length    : 128 (字节, 即 1024-bit DH)
Prime Modulus : ffffffff ffffffff c90fdaa2 2168c234 c4c6628b 80dc1cd1
               29024e08 8a67cc74 020bbea6 3b139b22 514a0879 8e3404dd
               ef9519b3 cd3a431b 302b0a6d f25f1437 4fe1356d 6d51c245
               e485b576 625e7ec6 f44c42e9 a637ed6b 0bff5cb6 f406b7ed
               ee386bfb 5a899fa5 ae9f2411 7c4b1fe6 49286651 ece65381
               ffffffff ffffffff
Server PubKey : 每连接随机 128 字节
```

结论：字段布局十年未变（`u16 g → u16 keyLen → modulus[keyLen] → pubkey[keyLen]`），
Apple 在升级时把 DH 从自选 1024-bit 素数换成了 RFC 5054 的 4096-bit SRP 组（g=5）。
**客户端应使用服务器下发的值而非硬编码**（nmap 实现即如此：`modulus = openssl.bignum_bin2bin(...)`）。

### 4.4 客户端计算与响应（nmap `login_ard` 参考实现，伪代码）

```text
# 1. DH 密钥对（私钥指数取 512-bit 随机数即可被服务器接受；严格实现可用 [2, p-2] 全域）
secret = random(512-bit)
cliPub = generator ^ secret  mod modulus

# 2. 共享密钥，左侧补零到 keyLength 字节（大端定长表示，前导 0x00 参与哈希！）
shared = serverPubKey ^ secret mod modulus
shared = zero_pad_left(shared, keyLength)

# 3. AES 密钥 = 共享密钥的 MD5（16 字节）
aesKey = MD5(shared)

# 4. 凭据明文块：128 字节 = username 补 NUL 到 64 + password 补 NUL 到 64
#    规范建议剩余空间填随机字节（降低相同明文→相同密文的可关联性；全 NUL 服务器也接受）
blob = pad64(username) || pad64(password)

# 5. AES-128-ECB 加密（无 IV、无填充——128 字节恰好 16 个 AES 块）
ciphertext = AES-128-ECB-Encrypt(aesKey, blob)

# 6. 发送：密文在前，客户端公钥在后（同样左侧补零到 keyLength 字节）
send(ciphertext || zero_pad_left(cliPub, keyLength))

# 7. 读 u32 结果：0 = 认证成功 → 继续 ClientInit/ServerInit；非 0 = 失败
```

### 4.5 要点与陷阱

| # | 要点 |
|---|---|
| 1 | **凭据是 Mac 的真实本地账号**（用户名+登录密码），不是 VNC 密码；账号需有「屏幕共享/远程管理」权限。Apple 官方也称之为 "Mac authentication / ARD authentication" |
| 2 | 用户名/密码各限 **64 字节**（含结尾 NUL）；超长即无效 |
| 3 | 共享密钥与客户端公钥都必须**左侧补零到 keyLength 字节**——DH 库的「最简表示」会丢前导零导致 MD5/A 长度不稳定（nmap 专门做了 `('\0'):rep(keylen - #shared) .. shared`） |
| 4 | AES 用 **ECB**：无 IV、无填充。第 5 步若用 CBC 或 PKCS#7 填充即失败 |
| 5 | 发送顺序是**密文在前、公钥在后** |
| 6 | 认证成功后**无需再发送任何 ARD 特有消息**，直接按标准 RFB 3.8 走 ClientInit（shared=1）→ ServerInit → SetPixelFormat/SetEncodings（与类型 2 完全一致） |
| 7 | 10.7+ 上用类型 30 认证可**绕过登录屏幕**直接控制已登录会话（类型 2 则会先看到登录屏幕）——这正是第三方客户端做类型 30 的主要价值（cafbit） |
| 8 | Apple 官方手册（ARD 2 时代）说明：控制会话中**键鼠事件可用认证派生的 128-bit AES 密钥加密**；但服务器同时接受标准（明文）RFB KeyEvent/PointerEvent，第三方客户端可以只发明文（barneygale 验证） |

### 4.6 兼容性风险（务必实测）

- **macOS 10.15 实测报告**（GUACAMOLE-1133）：Guacamole 1.2.0 选择 30 后，服务器回吐 `33, 36, 2, 35` 类型列表并断开，旧式 30 握手失败。意味着较新 macOS 对 30 的接受度可能受系统策略影响（例如要求目录服务账号/特定权限）。
- **macOS 26（2026）**：列表仍为 `[30, 33, 36, 2, 35]`（本机实测），30 仍在服务端代码中，但公开的第三方实现（TigerVNC/gtk-vnc/nmap 之后）多年未更新，真实可用性需要在目标 macOS 版本上验证。
- 若 30 不可用，工程上的稳妥路径仍是类型 2（本仓库现状）。

---

## 五、新一代认证（33/35/36 与 RSA-SRP）——已部分逆向（2026-08-18）

字节级格式**未公开**，任何实现都需自行逆向。以下为公开可查的事实拼图：

### 5.0 类型 36（SRP）——✅ 2026-08-18 已完整逆向并在本仓库实现（`src/vnc/srp.rs`）

对 macOS 26.6.1 `screensharingd` 反汇编 + 真机逐字段验证（含 H_AMK 服务器证明闭环）。
密码学骨架是 corecrypto 的 `ccsrp`（SRP-6a，RFC 5054 的 4096-bit 组，SHA-512），
外面包一层 Apple 私有 TLV 信封。

**消息信封与 TLV 项**：

```text
C→S 消息: [u8 36][u32 L][负载 L]，负载 = [u32 L-4][TLV 项…]（mech 校验内层长度头）
S→C 消息: [u32 L][u32 L-4][TLV 项…]
state 9（挑战后的 step2）消息不带 [36] 前缀，直接 [u32 L]…
TLV 项: %s=[u16 BE len][数据]  %o=[u8 len][数据]  %m=[u16][数据]  %q=8B BE  %c=1B
```

**四步握手**（✅ = 已实现验证）：

```text
✅ C→S 选择  [36]（单独一字节）
✅ C→S step1 [36][u32][TLV: s("")+s(用户名)+s("")+o("")]   ← 用户名在第二个字段！
✅ S→C 挑战  TLV: c(cmd) + m(素数p=RFC5054-4096) + m(g=5) + o(盐32B)
               + m(B) + q(PBKDF2迭代数) + s(选项串)
   选项串 = "mda=SHA-512,replay_detection,conf+int=ChaCha20-Poly1305,kdf=SALTED-SHA512-PBKDF2"
✅ C→S step2 [u32][TLV: m(A) + o(M1) + s(选项串原样回传) + o(随机16B nonce)]
✅ S→C 响应  [u32 92][u32 88][0x40][64B H_AMK][23B 尾部] → RFB u32 SecurityResult(0)
```

**密码学配方**（corecrypto `ccsrp_ctx_init` 三参默认 = `CCSRP_OPTION_SRP6a_HASH`，
即 KDF=H(S)、k 参与乘法、u 取全 64 字节摘要、所有大数按 512 字节左填充参与哈希）：

```text
dk    = PBKDF2-HMAC-SHA512(密码, 盐, 迭代数, dkLen=128)
x     = SHA512(盐 ‖ SHA512(":" ‖ dk))          ← 用户名不参与（noUsernameInX 变体）
k     = SHA512(N₅₁₂ ‖ g₅₁₂) → 整数
u     = SHA512(A₅₁₂ ‖ B₅₁₂) → 整数
S     = (B − k·g^x)^(a + u·x) mod N             ← a 为客户端随机私钥
K     = SHA512(S₅₁₂)                            ← 会话密钥材料（64B）
M1    = SHA512(H(N₅₁₂)⊕H(g₅₁₂) ‖ SHA512("") ‖ 盐 ‖ A₅₁₂ ‖ B₅₁₂ ‖ K)
        ← M1 里的"用户名"是空串常量（反汇编 0x1000112a3 处 lea 指向 ""）
H_AMK = SHA512(A₅₁₂ ‖ M1 ‖ K)                   ← 服务器证明，客户端应校验（防 MITM）
```

**反直觉细节**（每条都实测踩坑确认）：

1. **用户名在 step1 的第二个 %s 字段**——放第一个字段服务器会用空名查 OD，
   返回 **-20 → 诱饵挑战**（`CCRandomGenerateBytes` 假盐 + 假 verifier，防用户名枚举）；
2. 挑战里的 `%q` u64 = **PBKDF2 迭代数**（实测 0x2625a=156250，与账号 ShadowHash
   blob 里的值一致），不是选项掩码；
3. step2 的 %s 必须**原样回传选项串**（服务器从中解析 `mda=SHA-512` 选摘要算法，
   空串 → "Unable to find SRP MDA option" → mech -1）；
4. M1 紧跟 A（第二个字段 %o），选项串在第三，尾部 16B 随机 nonce（缺 nonce → 解析错）；
5. 服务器对连续失败认证有**递增延迟**（unified log: "bad auth count/delay"），
   调试时多次失败后会被临时限流/断连；
6. 认证失败一律返回 4B `00000001`（不区分凭据错/诱饵路径，防信息泄露）。

**与账号存储的关系**：ShadowHash 里的 `SRP-RFC5054-4096-SHA512-PBKDF2` blob =
`{盐 32B, 迭代数, verifier 512B}`，其中 `verifier = g^x mod N`（x 同上式）。
离线验证 oracle：`pow(5, x, N) == verifier`。

**会话加密**（未实现）：认证协商了 `conf+int=ChaCha20-Poly1305`（服务器侧
"going to encrypt everything that is sent"）；实测明文 RFB 会话仍被接受。


### 5.0.1 类型 33（RSA-SRP 混合）——✅ 2026-08-18 已完整逆向并在本仓库实现（`src/vnc/rsa_srp.rs`）

Apple 客户端默认优先选择的路径：RSA 公钥加密传输 step1，其后的 SRP 与类型 36 同构。

```text
✅ C→S 选型+v0  [0x21][u32 10][01 00 "RSA1"][u16 0][u16 0]   ← 必须同帧；长度恰 10
✅ S→C 公钥     [u32 klen+6][u16 1][u16 0][u16 klen][SPKI DER]（RSA-2048）
✅ C→S v2 帧    [u32 L][01 00 "RSA1"][u16 2][u16 ctlen][RSA-PKCS1v1.5(step1 TLV)]
✅ S→C 挑战     [u32 L][u32 2][u16 M][u32 M-4][项…]           ← 项与 36 同构
✅ C→S step2    [u32 L][01 00 "RSA1"][u16 2][u16 M2][u32 M2-4][项]（明文）
✅ S→C 响应     [u32 98][u32 2][u16 92][u32 88][0x40][64B H_AMK][23B] + SecurityResult
```

- **RSA 填充 = PKCS#1 v1.5**（反汇编 `SecKeyDecrypt(key, 1, …)`，kSecPaddingPKCS1）；
- 服务器要求**同时广播类型 36** 才走此路径（在广播串中搜 `'$'` = 0x24 = 36；
  否则 "viewer requested RSA SRP but SRP was not advertised"）；
- SRP 数学、TLV 项、H_AMK 校验与类型 36 完全一致（复用同一实现）。

类型 35 = Kerberos；31/32 = DH Ask-User 变体（目录服务场景，复用类型 30 的 FUN_100074050）。

### 5.1 barneygale 逆向（2021-12，macOS Big Sur 时代）

Reddit r/ReverseEngineering 发帖 + Python PoC gist（使用 `cryptography` 库的 `load_der_public_key` / `padding` / AES `modes`）：

- 当前 Screen Sharing 认证使用 **2048-bit RSA 密钥 + 128-bit AES**；
- 服务器 RSA 公钥（DER）明文下发，客户端生成会话 AES 密钥后用 RSA 加密回传——**在明文中协商对称密钥**（作者原话 "also *weird*"）；
- **无重放攻击防护**；
- AES 密钥用于后续**击键等输入事件加密**；**图像数据明文传输**；
- 服务器同时接受不加密的标准（VNC 兼容）键鼠事件。

（gist 原文 `gist.github.com/barneygale/6b46b0eb7fd8adfd692ac4bf7816061c`，2026-08 检索时已无法访问；以上为帖内自述与 gist 首部片段。）

### 5.2 Huntress / CVE-2026-65400 披露（2026-08）

- 原生 Apple 认证路径实现为 **SRP（Secure Remote Password）**；合法登录在 macOS Endpoint Security 的 `ES_EVENT_TYPE_NOTIFY_SCREENSHARING_ATTACH` 事件中 `authentication_type` 为 **`RSA-SRP`**（漏洞路径则显示 `SRP` 且无加密保护）。
- `screensharingd` 的 SRP 帧解析存在两个独立逻辑漏洞（详见 §8.3）。
- 推论：**类型 33/35/36 ≈ RSA-SRP 家族的不同变体/版本**（33 与 36 的具体差异仍未知；35 在 2010 年已存在，可能是族中最早成员）。

### 5.3 对本项目的意义

- 想摆脱「Mac 上必须开启 VNC 密码」的限制（README 已知限制第 3 条），**类型 30 是唯一有完整公开实现路径的方案**；
- 33/35/36 需要自己抓包逆向（在自有设备上 mitm 5900 流量、对照 `screensharingd` 反汇编），工作量与法律风险都更高。

---

## 六、认证后的会话与 Apple 私有扩展

### 6.1 会话主体仍是标准 RFB

认证成功后，ServerInit/SetPixelFormat/SetEncodings/FramebufferUpdate 循环与标准 VNC 一致。
Apple 的扩展通过 **SetEncodings 里的私有编码（正数）/伪编码（负数）** 协商——这是 RFB 的标准扩展机制。

### 6.2 私有编码（已知线索）

| 线索 | 来源与说明 |
|---|---|
| 编码 `0x0000044C (1100)`、`0x0000044D (1101)` | Apple 社区帖（2010）：Screen Sharing.app 支持的未公开编码。具体语义至今无公开文档 |
| `1099–1110` 一带存在一批 Apple 私有（JPEG 相关）编码 | HN 讨论（Remotix 集成 Apple 专用 VNC 协议的评论）；Remotix（后开源部分代码）是少数逆向了这些编码的第三方 |
| **MVS / Apple Adaptive Codec** | ARD 高性能编码，内部名 Multi-Variant Stream：**每块图形更新分两遍渐进发送**（先快后精）；社区描述为基于 H.264 的自定义封装，配合服务器端 Retina 自适应降采样（Devolutions 论坛/博客） |
| High Performance 模式 | Apple Silicon 之间的「高性能屏幕共享」：立体声音频、HDR 参考模式、4:4:4 色度、30/60fps（Apple 官方文档）——完全私有，无第三方实现 |

**结论**：第三方客户端（含 RDM）实践中的通行做法是：认证用 Apple 私有类型，**像素传输退回标准编码（Raw/zlib/Hextile/Tight）**。RDM 博客明确指出：原生 VNC 的 zlib 压缩 Raw「粗陋低效」，Retina 屏下尤其慢——这正是 Apple 造 MVS 的原因，也是第三方不实现 MVS 时的性能代价。

### 6.3 击键加密

- Apple 客户端用认证阶段派生的 AES-128 密钥加密 KeyEvent/PointerEvent（ARD 2 起的官方行为）；
- 服务器**同时接受明文标准事件**（barneygale 实测：加密击键为默认行为，但服务器接受不加密的标准 VNC 兼容键鼠事件）——本仓库 viewer.rs 的明文路径在类型 2 下已验证可用，在类型 30 下理论同样可用。

### 6.4 其他会话特性（RDM 逆向所得）

Devolutions（Remote Desktop Manager，**即本项目用户正在使用的客户端**）完整逆向 ARD 协议后实现：

- 加密输入事件、正确的显示缩放（Retina 半分辨率协商）、多显示器支持、**Curtain mode（远端锁屏+遮蔽，本地照常控制）**；
- ARD 协议**不支持声音重定向**（Devolutions 论坛确认）；
- RDM 的 ARD 客户端跨 Windows/macOS/Linux/iOS/Android 及 Web（Devolutions Gateway 内置 ARD web 客户端）。

### 6.5 对 FreeRemoteDesk 有直接影响的两个 quirk（cafbit）

1. **右键映射**：ARD 3.5（Snow Leopard，2011-07 更新）把 RFB `button-2` 当右键（标准 VNC 用 `button-3`=bit2）。本仓库 `viewer.rs` 目前按标准发 bit2=右键，若目标 Mac 表现异常可尝试切换。
2. **键盘注入的根因**：macOS 只支持注入**物理 keycode**，不支持注入符号级 **keysym**。AppleVNC 收到 keysym 后用「美式键盘的 keysym→keycode 对照表」翻译再注入，远端再按自己的键盘布局把 keycode 翻回字符——外国布局下打字错乱，且无法输入服务器键盘上不存在的字符。这解释了本仓库 README 已知限制「无法输入中文」的深层原因（minifb 无 IME 是客户端侧限制，keysym→keycode 有损翻译是服务器侧限制，两层叠加）。

---

## 七、3283 管理通道

ARD 管理端（报告/软件分发/开关机/锁屏/消息广播/Curtain）走 TCP/UDP 3283，与 5900 的屏幕会话相互独立。公开的字节级逆向资料非常稀少（远少于 5900 认证部分），本仓库场景（屏幕查看/控制）用不到，不展开。需要注意：

- 3283 与 5900 在系统设置里是同一个「远程管理/屏幕共享」开关族；
- 安全审计角度，开了屏幕共享通常意味着 3283 也暴露（Apple 防火墙默认放行内置服务）。

---

## 八、安全性分析

### 8.1 类型 30 的密码学弱点

| 弱点 | 分析 |
|---|---|
| **无服务器认证的 DH** | 客户端无法验证服务器身份，活动 MITM 可冒充服务器截获凭据块（只能离线爆破 AES，但凭据块结构固定 username[64]‖password[64]，已知明文量大） |
| 静态/共享素数 | 1024-bit 且全网同值；1024-bit 离散对数对国家级攻击者已属可行区间（Logjam 级别评估） |
| MD5 做 KDF | 碰撞弱点对 KDF 场景影响有限，但属于弃用算法 |
| AES-ECB 无完整性 | 无 MAC/认证标签，密文可被篡改（64/64 定长结构下利用面小，但不符合现代 AEAD 实践） |
| 无重放防护 | 同一 (cliPub, ciphertext) 可重放（凭据不变则密文不变——随机填充 blob 尾部可部分缓解） |

### 8.2 类型 2 的既有弱点（本仓库已实现侧）

- 密码仅前 8 字节参与 DES 挑战响应；挑战-响应本身无服务器认证，同样可 MITM（凭据是独立 VNC 密码，泄露面较小）；
- 会话全程明文（图像+键鼠）。**键鼠明文**在类型 2 下是默认状态——局域网之外使用务必套 SSH 隧道/VPN（SANS、Apple 官方均如此建议）。

### 8.3 CVE 时间线（2026 年 8 月，与本协议直接相关）

| 日期 | 事件 |
|---|---|
| 2026-07-27 | Apple 常规更新 26.6/15.7.8/14.8.8：修 CVE-2026-43760 等 3 个 Screen Sharing 漏洞；**顺带（未在公告中说明）关闭了 @osxreverser 私自发现未报告的一个 pre-auth 远程 root（无 CVE）** |
| 2026-07-29 | Bynario 公开 CVE-2026-43760 writeup（post-auth：legacy VNC 认证路径下，`SSFileCopySender/Receiver` 以 root 运行 → 认证用户可以 root 读写任意文件）；同日 @osxreverser 发文 "It's a pre-auth, stupid!" 并放出混淆 PoC（以 root 下载任意文件，继承 `kTCCServiceSystemPolicyAllFiles` 全盘访问 entitlement） |
| 2026-08-01/02 | bl4sty 逆向 PoC 还原线格式，定位到 **SRP 帧长度校验器返回陈旧的成功状态码**，扩展出读写+RCE（cron 持久化路径仅 SIP 关闭目标可用） |
| 2026-08-06 | Apple 紧急发布 26.6.1/15.7.9/14.8.9，修 **CVE-2026-65400**（同文件中第二个独立逻辑漏洞：认证状态机失步；pre-auth、只需知道一个用户名即可冒充任意账号；CVSS 随后从 7.1 上调至 9.8 critical） |
| 2026-08-08 | 野外利用确认（植入 Monero 挖矿）；互联网上约 **4 万台**开放 Screen Sharing 的 Mac（近半在美国） |

两个漏洞都是**纯逻辑 bug**（无堆喷射/无竞态/一次性稳定触发），根源是「在 VNC 上外挂私有认证体系」的复杂度。这对协议分析者的启示：`screensharingd` 的认证状态机本身就是这块攻击面最大的软肋。

### 8.4 加固建议（自有 Mac mini 场景）

1. 保持 macOS 更新（≥ 26.6.1 / 15.7.9 / 14.8.9）；
2. 屏幕共享不直接暴露公网：置于 VPN/防火墙规则之后，或 SSH 隧道（`ssh -L 5900:localhost:5900`）；
3. 仅授权必要用户；关闭「VNC 显示程序可以使用密码控制屏幕」（去掉弱类型 2）；
4. macOS 防火墙开启 `setallowsigned off`（自动放行签名软件会让 5900 直通）；
5. 检测侧：ES 框架订阅 `ES_EVENT_TYPE_NOTIFY_SCREENSHARING_ATTACH`，关注 `session_username: root`、`authentication_type: SRP`（非 RSA-SRP）。

---

## 九、类型 30 的实现（✅ 2026-08-18 已完成并真机验证）

> 状态：`src/vnc/ard.rs` 已实现；`info/shot/view` 从非回显的
> `FRD_USERNAME`/`FRD_PASSWORD` 环境提供器读取凭据，命令行不接受明文凭据。
>（截图 3.4MB 真实画面）。原路线图保留如下供参考。

### 9.1 目标与收益

- 免去「Mac 必须启用 VNC 密码」（README 已知限制 3），直接用 Mac 账号登录；
- 绕过 10.7+ 类型 2 路径的登录屏幕。

### 9.2 新增依赖（全部 RustCrypto 系，无 C 依赖）

```toml
num-bigint-dug = "0.8"   # 或 num-bigint，提供 modpow
md-5  = "0.10"
aes   = "0.8"
```

### 9.3 代码骨架（放在 src/vnc/ard.rs，中文注释风格与仓库一致）

```rust
//! ARD 认证（安全类型 30）：DH-1024 + MD5 派生 + AES-128-ECB 凭据块。
pub fn ard_authenticate(
    conn: &mut RfbConn,
    username: &str,
    password: &str,
) -> Result<()> {
    // 1. 读服务器 DH 材料：u16 g、u16 keyLen、modulus、serverPub
    let g = conn.read_u16()? as u32;
    let key_len = conn.read_u16()? as usize;
    if !(64..=512).contains(&key_len) { bail!("异常的 DH 长度 {key_len}"); }
    let modulus  = BigUint::from_bytes_be(&conn.read_vec(key_len)?);
    let srv_pub  = BigUint::from_bytes_be(&conn.read_vec(key_len)?);
    let g = BigUint::from(g);

    // 2. 客户端密钥对与共享密钥（定长大端，左侧补零参与哈希）
    let secret = random_biguint_below(&modulus);          // 用 os randomness
    let cli_pub = g.modpow(&secret, &modulus);
    let shared  = srv_pub.modpow(&secret, &modulus);
    let shared_bytes = pad_left(&shared.to_bytes_be(), key_len);

    // 3. AES 密钥 = MD5(共享密钥)
    let aes_key = md5::Md5::digest(&shared_bytes);

    // 4. 凭据块：username[64] || password[64]，NUL 结尾+随机填充
    let mut blob = [0u8; 128];                             // 先全 0（服务器接受）
    put_field(&mut blob[..64], username);
    put_field(&mut blob[64..], password);

    // 5. AES-128-ECB（无填充，128 字节恰好 8 块）
    let enc = Ecb::<Aes128, DummyPadding>::new_from_slice(&aes_key)?;
    let mut buf = blob;
    enc.encrypt_padded_mut::<NoPadding>(&mut buf, 128)?;

    // 6. 密文在前，公钥在后（同样左侧补零）
    conn.write_all(&buf)?;
    conn.write_all(&pad_left(&cli_pub.to_bytes_be(), key_len))?;

    // 7. u32 结果
    if conn.read_u32()? != 0 { bail!("ARD 认证失败（检查用户名/密码与屏幕共享权限）"); }
    Ok(())
}
```

### 9.4 集成点

| 文件 | 变更 |
|---|---|
| `src/vnc/protocol.rs` | `security` 模块已有 `APPLE_ARD = 30` 常量（已存在）；`security_type_name` 补 33/36 命名 |
| `src/vnc/client.rs` | `pick_security()`：提供用户名+密码时依次选择 36、33、30，再回退 2；不存在环境变量强制认证类型或明文降级后门 |
| `src/main.rs` | `info/shot/view` 只读取 `--username-env`/`--password-env` 指定的环境变量名，默认 `FRD_USERNAME`/`FRD_PASSWORD`；不接受凭据值参数 |
| 集成测试 | mock server 增加 type-30 分支：服务端侧做同样 DH/AES 计算验证客户端响应（自校验往返），凭据断言用户名/密码正确解出 |

### 9.5 测试与验证顺序

1. 单测：DH/AES 用已知向量自往返（客户端加密→mock 服务端解密比对 blob）；
2. 通过非回显凭据提供器设置环境变量后实测 `info <host>`（先确认该账号有屏幕共享权限）；
3. 若 10.15+ 出现 §4.6 的「选 30 被回吐类型列表」，降级逻辑自动改走类型 2 并提示。

### 9.6 当前证据边界

- 33/36 认证和 Apple 会话加密已实现；SRP 组、长度和计数器均 fail-closed 校验，生产路径不允许明文降级。
- MVS 完整帧、表初始化、generation 重置与严格重组已实现；type-1 部分更新仍因缺少可信 fixture 而只触发全量重同步。
- P4 已完成 Message 1、version-3 `0x1c`、version-2 Message 2、generation-bound UDP socket、SRTP/SRTCP 与有界真机互操作；P3 的 AAC-ELD 解码和 Windows 播放也已实证，二者不受 P5 身份门控结论影响。
- P5 的 mode-4 有界实验曾得到认证、重放接受且位于发送范围内的 SRTCP 报告；这只
  证明通用 AVConference 端点接收/报告报文，不证明 ARD 产品路径、解码、播放或远程
  输入设备。随后对 stock macOS 26.6.2 `ScreenSharing.framework` 的离线 Ghidra 恢复
  给出了排他性产品门控：`audioChatSupported` 仅在 `idsSession` 非空或地址为 Apple-ID
  邀请时为真；`setAudioChatMuted:` 也只分流到 QR/IDS AVConference、legacy IDS 或
  invitation agent。ARD 3.10 主程序没有 Audio Chat 控制路径。因此用户名/密码 HPSS
  下的 P5 在当前项目规则内是 **不支持**，而不是等待更多试听的功能。
  `--udp-audio-input` 在网络会话选择前 fail-closed，Windows 麦克风不会打开；mode-4
  代码和 fixture 仅保留为离线逆向证据。详见 `ard_re/P5_PROTOCOL_ANALYSIS.md`。
- 不根据相邻字符串或数值套件猜测编解码器、HDR、帧率或加密算法。

---

## 十、参考资料

**类型 30 认证（字节级）**

1. David Simmons — *Apple Remote Desktop quirks*（2011-09，原始逆向分析，gtk-vnc 补丁还原）
   <https://cafbit.com/post/apple_remote_desktop_quirks/>
2. Stack Overflow — *Authentication process in ARD*（2011，逐步骤描述 + Java/ObjC 实现指引）
   <https://stackoverflow.com/questions/6938432/authentication-process-in-ard>
3. Tenable — *Detecting macOS High Sierra root account without authentication*（2017-11，实抓报文：generator 0002 / keyLen 128 / 完整模数 hex）
   <https://www.tenable.com/blog/detecting-macos-high-sierra-root-account-without-authentication>
4. nmap `nselib/vnc.lua::login_ard()`（可直接运行的 Lua 参考实现，引用 cafbit）
   <https://raw.githubusercontent.com/nmap/nmap/master/nselib/vnc.lua>
5. nmap `scripts/vnc-info.nse`（把 30/35 命名为 "Mac OS X security type"、收录 RFB 003.889 指纹）
   <https://nmap.org/nsedoc/scripts/vnc-info.html>

**安全类型 / 新一代认证**

6. Apple 社区讨论 — *ARD Protocol*（2010，类型列表 30/2/35 + 未公开编码 0x44C/0x44D）
   <https://discussions.apple.com/thread/2676183>
7. gtk-vnc issue #34 — *Unknown auth type: 33*
   <https://gitlab.com/GNOME/gtk-vnc/-/issues/34>
8. barneygale — *macOS VNC authentication*（r/ReverseEngineering，2021-12；2048-bit RSA + 128-bit AES、密钥明文协商、无重放防护、击键加密/图像明文）
   <https://www.reddit.com/r/ReverseEngineering/comments/rogfxj/macos_vnc_authentication/>
9. GUACAMOLE-1133 — *VNC fails to connect to macOS*（2020，10.15 上选 30 被回吐 `[33,36,2,35]` 的兼容性报告）
   <https://issues.apache.org/jira/browse/GUACAMOLE-1133>
10. noVNC issue #58 — *Broken with Apple Remote Desktop/Screen Sharing*（RFB 3.889 非标准版本号）
    <https://github.com/novnc/noVNC/issues/58>
11. wayvnc issue #277 — *Support standard VNC protocol authentication*（Apple 客户端与非标准安全类型的互操作困境）
    <https://github.com/any1/wayvnc/issues/277>

**会话扩展 / 产品视角**

12. Devolutions — *Spotlight on: Apple Remote Desktop (ARD) in Remote Desktop Manager*（2024-11：RDM 完整逆向 ARD、MVS 渐进两遍编码、加密输入事件、curtain mode、无声音重定向）
    <https://devolutions.net/blog/spotlight-on-apple-remote-desktop-ard-in-remote-desktop-manager/>
13. Devolutions 论坛 — *Apple Remote Desktop* 主题（MVS = Multi-Variant Stream 内部名）
    <https://forum.devolutions.net/topics/34130/apple-remote-desktop>
14. Apple 官方 — *Encrypt network data in Remote Desktop*（ARD 2 时代高层描述：DH + AES、键鼠事件加密）
    <https://support.apple.com/guide/remote-desktop/encrypt-network-data-apdfe8e386b/mac>

**安全性 / CVE**

15. Huntress — *From Screen Share to Root Access: CVE-2026-43760 & CVE-2026-65400*（2026-08-07：RSA-SRP、SSFileCopySender root+全盘访问、检测与补丁矩阵）
    <https://www.huntress.com/blog/macos-screen-sharing-rce-patched>
16. Calif — *No Country for Old Passwords*（2026-08-10：两个 pre-auth 远程 root 的发现史与时间线）
    <https://blog.calif.io/p/no-country-for-old-passwords>
17. SANS ISC — *Apple Screen Sharing Security*（2026-08-17：双认证路径风险与加固命令）
    <https://isc.sans.edu/diary/Apple+Screen+Sharing+Security/33252/>
18. Malwarebytes / Ars Technica / Tom's Hardware（2026-08：CVE-2026-65400 野外利用、Monero 挖矿、约 4 万台暴露）
    <https://www.malwarebytes.com/blog/bugs/2026/08/update-your-mac-screen-sharing-vulnerability-exploited-in-the-wild>

---

*本文档由公开资料整理，用于互操作开发与防御性安全研究；仅可用于自有/授权网络与设备。*
