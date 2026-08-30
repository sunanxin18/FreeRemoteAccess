# ARD 会话层协议规范（逆向还原）

> 本文档为 FreeRemoteDesk 项目对 macOS Screen Sharing（screensharingd，macOS 26.6.1 arm64）
> 会话层私有协议的逆向记录。内容按已验证证据和明确受限的实现分层；未由反汇编、捕获或
> 真机实验交叉验证的字段不得视为协议规范。
>
> 姊妹文档：`ARD_PROTOCOL.md`（认证层：类型 2/30/33/36）。
> 本文档覆盖**认证成功之后**的一切：会话初始化、能力协商、动态加密、换钥协议、帧格式。

---

## 目录

1. [术语与常量定义](#一术语与常量定义)
2. [连接全流程状态机](#二连接全流程状态机)
3. [ClientInit 能力字节](#三clientinit-能力字节)
4. [SetEncodings 消息（类型 0x12 族）](#四setencodings-消息类型-0x12-族)
5. [SetEncryption 消息（cmd=1/2）](#五setencryption-消息cmd12)
6. [换钥记录协议](#六换钥记录协议)
7. [EncryptOneMessage 帧格式](#七encryptonemessage-帧格式)
8. [服务器内部架构（双缓冲）](#八服务器内部架构双缓冲)
9. [会话选择（SessionSelect）](#九会话选择sessionselect)
10. [消息类型总表（state 4 分发器）](#十消息类型总表state-4-分发器)
11. [证据索引与用例](#十一证据索引与用例)
12. [逆向工具与脚本清单](#十二逆向工具与脚本清单)

---

## 一、术语与常量定义

### 1.1 枚举：安全类型（认证层，见 ARD_PROTOCOL.md）

```rust
/// RFB 安全类型（客户端在类型列表中选择）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityType {
    None            = 1,   // 无认证（macOS 不提供）
    VncDes          = 2,   // 标准 VNC DES 挑战-响应（RFC 6143 §7.2.2）
    ArdDh           = 30,  // Apple DH + AES-128-ECB 凭据块
    DhAskUserA      = 31,  // DH Ask-User 变体（目录服务，复用类型 30 的 DH+MD5）
    DhAskUserB      = 32,  // 同上，另一状态机分支
    RsaSrp          = 33,  // RSA 密钥传输包裹的 SRP（Apple 客户端默认路径）
    PreAuthorized   = 34,  // 预授权连接
    Kerberos        = 35,  // Kerberos 票据认证
    Srp             = 36,  // 纯 SRP-6a（RFC 5054 4096 组 + SHA-512）
}
```

### 1.2 枚举：会话状态（viewer+0x1c，服务器状态机）

```rust
/// 服务器会话状态（FUN_100038248 的 switch 分发值）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerState {
    ProtocolVersion = 0,  // 等待 RFB 版本 banner（"RFB 003.889\n"）
    Authentication  = 1,  // 认证类型选择与凭据交换（FUN_100013f78）
    InitProcessing  = 2,  // ClientInit 处理 + 偏好/能力评估 + ServerInit 发送
    SessionSelect   = 3,  // 会话选择阶段（Fast User Switching 场景，FUN_100065df7）
    MessageLoop     = 4,  // 常规消息循环（明文 RFB 分发器，FUN_10003a47c）
}
```

**证据**：反汇编 `0x10003826a: MOV EAX,[RDI+0x1c]; CMP RAX,0x4; JA default`，
状态写入点 `0x100039a38 (state=3)` / `0x10003a2ad (state=4)` / `0x10003a2c4 (state=1)`。

### 1.3 枚举：加密命令（SetEncryption 消息的 cmd 字段）

```rust
/// SetEncryption 消息的命令字（case 0x12 处理器，HandleSetEncryptionMessage）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionCmd {
    /// 协商加密算法：负载 = [u8 算法个数 N][N × u32 BE 算法 ID]
    /// 服务器遍历算法 ID，值==1(AES) 时触发密钥生成与换钥记录下发
    NegotiateAlgorithms = 1,
    /// 开启/关闭解密开关：负载 = [u8 flag]（1=服务器对后续收到的内容全部解密）
    SetDecryptFlag      = 2,
}
```

### 1.4 枚举：加密算法 ID（cmd=1 的算法列表值）

```rust
/// cmd=1 负载中的算法标识（唯一已知值）
pub const CIPHER_AES: u32 = 1;  // AES-128（CBC/ECB，由服务器 SetupAESKeys 建立）
```

### 1.5 枚举：Apple 私有编码（SetEncodings 编码值）

```rust
/// Apple 私有编码/伪编码（SetEncodings 可请求的值）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppleEncoding {
    // 标准 RFB
    Raw         = 0,
    CopyRect    = 1,
    Zlib        = 6,
    Tight       = 7,
    CursorPseudo = -239,      // 0xFFFFFF11，光标形状伪编码

    // Apple 私有（1000-1110 段）
    ApplePriv1002 = 1002,     // 语义未知（Apple 客户端请求）
    ApplePriv1011 = 1011,     // 语义未知
    MvsBaseline   = 1100,     // 0x44C，MVS 基线（多变化流）
    MvsEnhanced   = 1101,     // 0x44D，MVS 增强
    DeviceInfo    = 1103,     // 0x44F，设备信息（位打包，携带显示配置）
    MvsProfile4   = 1104,     // 0x450
    ApplePriv1105 = 1105,
    ApplePriv1107 = 1107,
    ApplePriv1109 = 1109,
    ApplePriv1110 = 1110,
}
```

**证据**：MITM 捕获 Apple 客户端 SetEncodings 原始字节（消息 [13]），
及 screensharingd 日志 `unable to set encoding profile %d`。

---

## 二、连接全流程状态机

```
┌─────────────────────────────────────────────────────────────────┐
│ 完整时序（36 SRP + 加密会话为例）                                │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│ C→S  "RFB 003.008\n"                    版本回显（state 0）     │
│ S→C  安全类型列表 [30,33,36,2,35]                              │
│ C→S  [36]                                选择 SRP（state 1）    │
│ C→S  step1（用户名在第二个 %s 字段）                            │
│ S→C  SRP 挑战（素数/g/盐/B/迭代数/选项串）                      │
│ C→S  step2（A + M1 + 选项串回传 + 16B cIV）                    │
│ S→C  H_AMK 响应 + SecurityResult=0                             │
│                                                                 │
│ C→S  ClientInit = 0xC1                   ★ 0x40 位=会话选择能力  │
│ S→C  ServerInit（分辨率/名称/像素格式）    （state 2）           │
│                                                                 │
│ C→S  [13] SetEncodings（Apple 编码族）    （state 4）           │
│ S→C  DeviceInfo(1103) 位打包 + 初始换钥记录 × N                 │
│                                                                 │
│ C→S  SetEncryption cmd=1 [算法ID=1]      ★ 触发服务器密钥生成    │
│ S→C  换钥记录 × M（服务器主动重钥）                              │
│ C→S  SetEncryption cmd=2 flag=1          ★ 开启双向加密         │
│ S→C  换钥记录 × K + "going to encrypt everything"               │
│                                                                 │
│ ── 此后所有 C→S/S→C 消息均为 EncryptOneMessage 帧 ──            │
│                                                                 │
│ C→S  加密帧（内含 RFB 标准消息：帧请求/键鼠/像素格式）          │
│ S→C  加密帧（视频数据/光标/设备信息）                            │
└─────────────────────────────────────────────────────────────────┘
```

---

## 三、ClientInit 能力字节

### 3.1 字段定义

```rust
/// ClientInit 消息（认证成功后客户端发送的 1 字节）
/// RFB 6143 定义 bit0 为 shared-flag；Apple 扩展了高位作为能力宣告
#[derive(Debug, Clone, Copy)]
pub struct ClientInitFlags {
    /// bit0: 共享会话（允许与其他观察者共存）
    pub shared: bool,
    /// bit6 (0x40): ★ 客户端支持会话选择（SessionSelect）消息
    ///   置 1 → 服务器日志 "send session select info to viewer"
    ///   置 0 → "do NOT send session select info to viewer"（默认）
    ///   影响：viewer+0xf75 标志 → 决定 state 2 → 3 还是 → 4
    pub supports_session_select: bool,
    /// bit7 (0x80): 请求独占会话（Apple 客户端恒置位，0xC1 = shared|0x40|0x80）
    ///   影响：viewer+0x19 = 1（具体行为待进一步逆向）
    pub exclusive_hint: bool,
}

impl ClientInitFlags {
    pub const APPLE_CLIENT: u8 = 0xC1;  // shared | session_select | exclusive
    pub const PLAIN_SHARED: u8 = 0x01;  // 仅 shared（第三方客户端默认）
}
```

### 3.2 取值影响表

| 值 | bit7 | bit6 | bit0 | 服务器行为 |
|---|---|---|---|---|
| 0x01 | 0 | 0 | 1 | "do NOT send session select info"，直接 state 4 |
| 0x41 | 0 | 1 | 1 | "send session select info"，可能进 state 3 |
| 0x81 | 1 | 0 | 1 | 同 0x01 + viewer+0x19=1 |
| **0xC1** | 1 | 1 | 1 | Apple 客户端标准值：会话选择能力宣告 |

**证据**：
- 反汇编 `0x1000385a8` 附近：`if (+0x18 != 0 && (byte & 0x40)) viewer+0xf75 = 1`
- 真机日志实验矩阵（CI=01/41/81/C1/E1/C0，`ard_re/enc_probe.py CI=` 环境变量）

### 3.3 SessionSelect_Needed 的最终决定因素

```rust
/// state 3 进入条件（0x10003a2b）：
///   viewer+0xf76 != 0 → state 3（SessionSelect 阶段）
///   viewer+0xf76 == 0 → state 4（常规消息循环）
///
/// f76 = f75 && FUN_10006592d()
/// 其中 FUN_10006592d 检查：
///   - viewer+0x1019 == 0 → return true（非 Kerberos 认证路径恒真）
///   - 否则走 OpenDirectory 查询控制台用户状态
///
/// 实测：单控制台用户 Mac 上，即使 CI=0xC1，SessionSelect_Needed=false
/// → 常规配置下始终 state 4（Apple 客户端也一样）
```

---

## 四、SetEncodings 消息（类型 0x12 族）

### 4.1 线格式

```rust
/// SetEncodings（RFB 标准 + Apple 扩展编码值）
/// 线格式：[u8 消息类型 = 0x12][u8 pad][u16 BE N][N × i32 BE 编码值]
///
/// Apple 客户端实际发送（MITM 捕获消息 [13]，72 字节）：
///   类型 0x12, pad 0x00, N = 0x0001, 后跟 17 个 i32 编码值
///   编码列表：Raw, Zlib, 0x0D, 1011, 1002, 6, 16, -239,
///             1104, 1100, -223(0xFFFFFF21), 1101, 1105, 1107, 1109, 1110
```

### 4.2 Apple 客户端完整编码表（实测字节）

```
1200 0001 0001 0001 0000 0001 0a00 0001
0200 000d 0000 03f3 0000 03ea 0000 0006
0000 0010 ffff ff11 0000 0450 0000 044c
ffff ff21 0000 044d 0000 0451 0000 0453
0000 0455 0000 0456
```

分解：
| 偏移 | 字段 | 值 | 含义 |
|---|---|---|---|
| 0 | u8 | 0x12 | 消息类型（SetEncodings 变体） |
| 1 | u8 | 0x00 | 填充 |
| 2-3 | u16 BE | 0x0001 | 计数域（Apple 可能用不同语义） |
| 4+ | i32 BE × 17 | 见枚举 1.5 | 编码值列表 |

**注意**：标准 RFB SetEncodings 的类型是 0x02；Apple 使用 0x12（18）。
分发器 case 0x12 处理（与 SetEncryption 共用入口，通过子字段区分）。

---

## 五、SetEncryption 消息（cmd=1/2）

### 5.1 cmd=1（算法协商 + 编码表下发）

```rust
/// 线格式（2026-08-20 抓包比对定案，72B 实测）：
///   [u8 0x12][u24 BE cmd][u16 BE param][u16 BE count][count × u32 BE 算法ID]
///   后接编码表消息（0x0a 族，与协商同帧发送）
///
/// 实测字节（协商 AES，Apple 编码表）：
///   12 000001 0001 0001 00000001 | 0a 00000102 0000000d 000003f3 ...
///   = 0x12 + cmd=1 + param=1(全部加密) + 1 个方法 + AES(1)
///
/// 实测字节（协商 AES，Raw-only 编码表 → 服务器回标准 RFB Raw 矩形流）：
///   12 000001 0001 0001 00000001 | 0a 00000102 00000001 00000000
///
/// 服务器行为（HandleSetEncryptionMessage case 0x12，0x100042d14 起）：
///   1. 遍历 count 个算法 ID（字节序转换后判断）
///   2. 值==1(AES) 时：
///      viewer+0x8ca = 1     （换钥标志，下次 SendFrameBuffer 触发 EncodeEncryptionInfo）
///      viewer+0x8cc = 1     （换钥计数器初始化）
///      viewer+0x8d0 = param （算法参数存储）
///      AuthGetRandomBytes(viewer+0x8e2, 16)  （生成新 IV，FUN_10006dcd1 = /dev/random）
///      AuthGetRandomBytes(viewer+0x8d2, 16)  （生成新钥）
///      FUN_100022ef8()      （唤醒发送线程 → 下发 52B EncryptionInfo，见第六节）
///   3. 无匹配算法时日志 "no valid encryption method found"
///
/// ★ 注意：编码表只能随 cmd=1 同帧发送；会话层内单独发 SetEncodings 会被服务器断连
/// ★ 编码表内容决定帧内矩形形态：Apple 编码表 → Apple 伪编码矩形（zlib）；
///   Raw 表 → 标准 RFB Raw 矩形流（一个矩形可跨多个帧拼接）
```

### 5.2 cmd=2（解密开关）

```rust
/// 线格式（2026-08-20 抓包比对定案，8B 实测）：
///   [u8 0x12][u24 BE cmd=2][u16 BE param][u16 BE 0]
///
/// 实测字节（开启）：
///   12 000002 0001 0000
///
/// 服务器行为：
///   param==1: viewer+0x8f2 = 1  → "**going to decrypt everything that is received"
///             agent+0x548 = 1   （通知代理层）
///   param==0: viewer+0x8f2 = 0  → 恢复明文接收
///
/// ★ 一旦 param=1，客户端后续所有消息必须为 EncryptOneMessage 帧格式（第七节），
///   否则服务器将密文头当明文消息类型分发 → 协议错误 → 断连
/// ★ cmd=2 后需等待 ≥600ms 让服务器完成会话状态迁移（3→4）并吐出初始突发，
///   过早发送应用层帧会被直接断连（实测 <600ms 必断）
```

### 5.3 服务器发送侧加密激活

```rust
/// 换钥完成后（"sent new encryption info" 日志）：
///   viewer+0x8f3 = 1  → "**going to encrypt everything that is sent"
///   条件：viewer+0x8d0 == 1（cmd=1 时设置的算法参数）
///
/// 此后服务器所有发送均走 EncryptOneMessage 帧
```

---

## 六、换钥记录协议

### 6.1 EncryptionInfo（52B 密钥下发消息，★ 2026-08-20 实测定案）

```rust
/// cmd=1(AES) 后服务器明文下发（不经帧加密）：
///   [16B 头 = 00000001 0000000000000000 0000044f]
///   [4B  BE 换钥计数器（viewer+0x8cc，cmd=1 时初始化为 1，实测恒为 1）]
///   [16B ECB_key16(新钥)   ← viewer+0x8d2]
///   [16B ECB_key16(新 IV)  ← viewer+0x8e2]
/// 总长 52 字节；ECB 钥 = k₀ = SHA256(K)[0..16]
///
/// 客户端解出 (new_key, new_iv) 后：
///   1. 双方 cryptor 重建（见 6.3）
///   2. 服务器发送帧计数器（viewer+0x914）置 0
///   3. 此后所有消息 = EncryptOneMessage 帧（第七节）
///   4. cmd=2 后服务器立即开始推加密帧（ServerState 突发）
///
/// 内部构造（SendFrameBuffer 的 EncodeEncryptionInfo 块，0x100024398-0x100024473）：
///   66B 缓冲：[+0x1e]=BE32 计数器、[+0x22]=新钥（用旧 ECB cryptor +0x600 原地加密）、
///   [+0x32]=新 IV（同样原地加密）；[+8]=0x34(52) 为线长度，经 FUN_10001ec5e 发送
///
/// ★ 之前误判为 "[u16 32][32B] 换钥记录" 的线上数据实际是 EncryptOneMessage 帧；
///   128B/64B/80B/96B 等大记录 = 初始 ServerState 突发的加密帧
```

### 6.2 密钥链（★ 完整定案：真机 11/11 帧 SHA1 验证 + 744 帧 8.3MB Raw 流验证）

```rust
/// SRP-6a（类型 36）会话密钥：
///   K = SHA512(S₅₁₂)                ← 与 M1/H_AMK 计算所用相同（ccsrp KDF_HASH 变体）
///
/// 初始 AES-128 钥：
///   k₀ = SHA256(K)[0..16]           ← SetupAESKeys（0x100017f61）：4 个 cryptor
///                                     CBC-enc(+0x5f0, IV=0) / ECB-dec(+0x5f8) /
///                                     ECB-enc(+0x600) / CBC-dec(+0x608)，keyLen=16
///
/// 会话钥（EncryptionInfo 下发）：
///   new_key = ECB_decrypt(k₀, msg[20..36])
///   new_iv  = ECB_decrypt(k₀, msg[36..52])
///   → 双方以 (new_key, new_iv) 重建全部 cryptor
///
/// 帧加解密（第七节）：
///   发送 key = new_key，CBC 链式：首帧 IV = new_iv，其后 = 上一帧末密文块
///   SHA1 = SHA1(BE32(counter) ‖ 明文[0..padded-20])，counter 双向独立从 0 起
///
/// 密钥派生指令级证据（0x1000188dd-0x100018907，SendSRPChallenge）：
///   CC_SHA256(data=ccsrp_ctx+0x20+4n·8 (=ccsrp_ctx_K), len=64, out=rbp-0x50)
///   movaps 摘要前 16B → SetupAESKeys(key)；摘要后 16B 弃用
```

### 6.3 服务器重建 cryptor 的精确指令（反汇编 0x100024478-0x1000245a0）

```
100024478  release cryptor +0x5f0
10002448a  CCCryptorCreate(op=0(kCCEncrypt), alg=0(kCCAlgorithmAES),
           options=0(kCCOptionCBC), key=viewer+0x8d2(槽A),
           keyLen=16, iv=viewer+0x8e2(槽B), ..., &cryptor+0x5f0)
100024555  release cryptor +0x600
10002456a  CCCryptorCreate(op=0, alg=0, options=2(kCCOptionECB),
           key=viewer+0x8d2(槽A), keyLen=16, iv=NULL, ..., &cryptor+0x600)
100024620  release cryptor +0x5e8
           ...（+0x5e8 同 +0x5f0 参数，op=1(kCCDecrypt)）
```

---

## 七、EncryptOneMessage 帧格式

### 7.1 明文块结构（FUN_10005e9e7 = EncryptOneMessage）

```rust
/// 加密前的明文块布局
/// 总长度 = (原始数据长度 + 0x25) & !0xF   （向上取整到 16 字节边界）
///
/// ┌────────────────┬──────────────┬────────────┬───────────────┐
/// │ u16 BE 原长     │ 原始数据      │ 零填充      │ SHA1(20B)     │
/// │ (2 字节)        │ (原长 字节)   │ (变长)      │ (校验和)      │
/// └────────────────┴──────────────┴────────────┴───────────────┘
///
/// SHA1 覆盖范围 = 前面所有字节（[u16 原长][数据][填充]）
/// 填充长度 = 总长 - 2 - 原长 - 20
```

### 7.2 线帧格式（FUN_10001ec5e 发送侧）

```rust
/// 线上传输的完整帧
///
/// ┌──────────┬───────────────┬────────┬────────────┬───────────────┐
/// │ 8B 零     │ u32 BE L+2    │ u16 0  │ u16 BE L   │ CBC 密文(L B) │
/// └──────────┴───────────────┴────────┴────────────┴───────────────┘
///   L = 明文块总长（16 的倍数）
///
/// 简化视角（客户端解析器 FUN_100036c5e 视角）：
///   [u16 BE L][L 字节 CBC 密文]
///   → peek u16 → 等待 L+2 可用 → 读 L 字节 → DecryptOneMessage
```

### 7.3 解密与校验（FUN_10005eb9d = DecryptOneMessageWithComCryption）

```rust
/// 服务器解密流程：
/// 1. CCCryptorUpdate(cryptor=viewer+0x5e8, 密文, L, 原地输出)
/// 2. SHA1_Init/Update/Final over [明文块 .. 明文块+L-20]
/// 3. 比较计算的 SHA1 与明文块末 20 字节
///    不匹配 → "1a - packet checksum error" → 返回 0xfffffffd
/// 4. 检查 [u16 BE 原长] < L
///    不满足 → "1b packet plaintext size %d is wrong" → 返回 0x243
/// 5. 通过 → 提取 [2..2+原长] 作为消息注入 netbuf
```

### 7.4 CBC 链式状态

```rust
/// cryptor 为有状态（CCCryptorUpdate 跨调用保持链式）：
/// - 第一帧 IV = 0（cryptor 创建时的初始 IV）
/// - 后续帧 IV = 前一帧密文的最后一个 16 字节块
/// - 客户端实现：手动跟踪 last_ct_block 作为下一帧 IV
```

### 7.5 Apple 客户端实测帧样本（MITM 捕获）

| 消息索引 | 帧大小 | u16 L | 明文块长 | 用途推测 |
|---|---|---|---|---|
| [19] | 50B | 48 | 48 | 小命令（≤26B 数据） |
| [22] | 82B | 80 | 80 | 中等消息（≤58B 数据） |
| [23] | 34B | 32 | 32 | 小命令（≤10B 数据） |
| [24] | 34B | 32 | 32 | 小命令 |

---

## 八、服务器内部架构（双缓冲）

### 8.1 缓冲区分工

```rust
/// viewer 结构体中的两个关键缓冲：
///
/// viewer+0x08   : 明文 netbuf（状态机/分发器的数据源）
/// viewer+0x9d0  : 密文缓冲（socket 原始字节 → 解密读取器的数据源）
///
/// 数据流（加密开启 [viewer+0x8f2 == 1]）：
///   socket recv → 读全部可用字节
///   → NetBufferAddData(viewer+0x9d0, 原始字节)
///   → FUN_1000365bc 会话循环:
///       ├── 状态机（从 +0x8 读明文消息）
///       └── 解密读取器（从 +0x9d0 取帧 → 解密
///           → NetBufferAddData(+0x8, 明文+2, 原长)）→ 注入回状态机
///
/// 数据流（加密关闭 [viewer+0x8f2 == 0]）：
///   socket recv → 原始字节直接进 +0x8 → 状态机直接处理
```

### 8.2 会话主循环（FUN_1000365bc）

```rust
loop {
    if available(viewer+0x8) > 0 {
        loop {
            result = state_machine(viewer);     // FUN_100038248
            if result != 0 { break; }          // 错误或无更多数据
            if available(viewer+0x8) == 0 { break; }
        }
    }
    // 状态机无法继续时尝试解密读取器
    result = decrypt_reader(viewer);           // FUN_100036c5e
    if result != 0 { return result; }          // 错误
    // 循环
}
```

### 8.3 解密读取器（FUN_100036c5e）

```rust
fn decrypt_reader(viewer) -> Result {
    let netbuf_cipher = viewer.field_0x9d0;

    // 1. 等待至少 18 字节可用
    if available(netbuf_cipher) < 18 { return Ok; }

    // 2. peek 前缀获取帧长
    let l = peek_u16_be(netbuf_cipher);
    if l + 2 > available(netbuf_cipher) { return Ok; }  // 等待完整帧

    // 3. 分配帧缓冲（首次时 malloc 到 viewer+0x920/+0x928）
    // 4. 读 L 字节密文到 +0x928
    read(netbuf_cipher, viewer.field_0x928, l);

    // 5. 解密（原地）
    let orig_len = DecryptOneMessage(
        cryptor: viewer.field_0x5e8,
        buffer: viewer.field_0x928,
        len: l,
    );
    viewer.counter_0x918 += 1;

    // 6. 注入明文到状态机 netbuf
    let plaintext = viewer.field_0x920;
    let msg_len = u16_be_at(plaintext);
    NetBufferAddData(
        dest: viewer.field_0x08,
        data: plaintext + 2,     // 跳过 [u16 原长]
        len: msg_len,
    );

    // 7. 循环（如果还有 ≥17 字节可用）
    if available(netbuf_cipher) >= 17 { goto step_2; }
}
```

---

## 九、会话选择（SessionSelect）

### 9.1 触发条件

```rust
/// state 3 进入条件（全部满足）：
/// 1. ClientInit bit6 (0x40) 置位 → viewer+0xf75 = 1
/// 2. FUN_10006592d() 返回 true：
///    - viewer+0x1019 == 0（非 Kerberos 路径恒真）
///    - 控制台用户存在（GetNameOfUserOnConsole != 0）
///    - 控制台会话 != 0xF8（0xF8 = loginwindow 空会话）
/// 3. 服务器偏好允许（CFPreferences 检查）
///
/// 实测：单用户 Mac mini 上 SessionSelect_Needed = false
/// （Apple 客户端也不走 state 3，除非存在 Fast User Switching 场景）
```

### 9.2 SessionSelect 消息（类型 0x21）

```rust
/// Apple 客户端在 ServerInit 后立即发送（MITM 消息 [12]，66B）
/// 线格式：[u8 0x21][u16 BE 载荷长 = 0x003E = 62][62B 载荷]
///
/// 载荷含显示配置字段（u16 对），具体字段含义待进一步逆向。
/// 重放此消息可触发服务器响应 DeviceInfo + 换钥记录。
```

---

## 十、消息类型总表（state 4 分发器）

```rust
/// state 4 分发器（FUN_10003a47c）的跳转表（0x1000450cc，38 个条目）
/// 类型值 → 处理器名称（从日志字符串推断）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMessageType {
    SetPixelFormat      = 0x00,  // HandleSetPixelFormatMessage
    SetEncodings        = 0x02,  // （标准 RFB）
    FramebufferUpdateReq = 0x03, // （标准 RFB）
    KeyEvent            = 0x04,  // （标准 RFB）
    PointerEvent        = 0x05,  // HandlePointerEventMessage
    ClientCutText       = 0x06,  // HandleViewerCutTextMessage（支持分段流式）
    ServerCutText       = 0x07,  // （不适用 C→S）
    // ... 标准类型 ...
    SetEncodingsVariant = 0x12,  // ★ Apple 扩展（含 SetEncryption cmd 1/2）
    SetDisplayConfig    = 0x20,  // HandleSetDisplayConfiguration
    SessionSelect       = 0x21,  // 会话选择请求（Apple 客户端初始消息）
    // ... 私有类型 ...
    // 类型 > 0x25 → 跳出分发器（不解密场景 = 协议错误）
}
```

---

## 十一、证据索引与用例

### 用例 A：36 SRP 认证 + 加密会话（✅ 已实现）

```
凭据：由非回显提供器写入 FRD_USERNAME / FRD_PASSWORD
RUST: freeremotedesk.exe view <host>
证据：真机截图 3.27MB（docs/ARD_PROTOCOL.md §5.0）
脚本：ard_re/srp_client.py
```

### 用例 B：36 SRP + 加密会话诊断链（✅ 已实现）

```
Python 实验台：ard_re/enc_probe.py
环境变量组合：
  CI=c1          ClientInit=0xC1（0x40 位=会话选择能力）
  CMD1=1         发送 SetEncryption cmd=1（协商 AES）
  SENDREQ=1      发送加密帧
  HOP=99         使用换钥链末跳钥
  INNER=pf/64/17/req  加密帧内层消息类型

已验证：
  ✓ 帧被服务器接受（无 decrypt/checksum 错误日志）
  ✓ 消息类型分发成功（HandleSetPixelFormatMessage 被调用）
  ✓ 服务器回 34B 加密帧（双向通道活）

当前 Rust 实现统一串行化 seal+write，严格验证 u16 帧长、AES 分组、SHA1、
双向计数器与规范填充；计数器或长度溢出直接失败，不回退明文。
```

### 用例 C：33 RSA-SRP 认证（✅ 已实现）

```
说明：生产 CLI 已移除 FRD_AUTH 强制类型后门；正常优先级会在服务器提供 36 时选择 36，
否则选择 33。凭据仍由 FRD_USERNAME / FRD_PASSWORD 提供。
RUST: freeremotedesk.exe view <host>
证据：真机认证成功 + 完整会话
脚本：ard_re/mitm36.py（捕获用）
```

### 关键日志 oracle（服务器统一日志）

```bash
# Mac 端实时查看（需要 --info 级别）
/usr/bin/log show --last 2m --info --predicate \
  'process == "screensharingd"' --style compact

# 关键日志行及其含义：
# "HandleSetEncryptionMessage cmd 1"      → cmd=1 被接受
# "HandleSetEncryptionMessage 2"          → cmd=2 被接受
# "**going to decrypt everything that is received" → 0x8f2=1 生效
# "**going to encrypt everything that is sent"     → 0x8f3=1 生效
# "sent new encryption info"              → 换钥记录已发出
# "update send encryption, old key %p"    → 换钥流程启动
# "1a - packet checksum error"            → 加密帧 SHA1 不匹配
# "1b packet plaintext size %d is wrong"  → 明文长度字段异常
# "packet decrypt error %d"               → CBC 解密失败
# "pref set for session select"           → 偏好检查通过
# "send/do NOT send session select info"  → f75 决定结果
# "SessionSelect_Needed %s"               → state 3 vs 4 决定
# "bitsperpixel %d" + "Only 16 or 32..."  → pf 消息处理结果
# "viewer result -2"                      → 连接将被关闭
```

---

## 十二、逆向工具与脚本清单

| 工具/脚本 | 位置 | 用途 |
|---|---|---|
| `mac.py` | ard_re/ | SSH/SFTP 到 Mac mini（SSHPW 环境变量） |
| `srp_client.py` | ard_re/ | 36 SRP 认证完整客户端（Python） |
| `sel_probe.py` | ard_re/ | 会话选择/SelectSession 编舞实验台 |
| `enc_probe.py` | ard_re/ | ★ 加密会话全流程实验台（环境变量驱动） |
| `mvs_probe.py` | ard_re/ | MVS/私有编码流捕获 |
| `mitm36.py` | ard_re/(Mac) | 类型列表重写 MITM 代理（捕获 Apple 客户端） |
| `oracle2.py` | ard_re/ | 离线 verifier oracle（x 公式验证） |
| `parse_macho.py` | ard_re/ | Mach-O 字符串交叉引用扫描 |
| `DumpAt.java` | ard_re/ | Ghidra headless 函数反编译导出 |
| `DumpDisasm.java` | ard_re/ | Ghidra headless 反汇编导出（带符号标注） |
| `FindCalls.java` | ard_re/ | Ghidra headless 函数调用点查找 |
| `stub_map.txt` | ard_re/ | stub→符号映射表 |

### Ghidra 项目

```
ard_re/ghidra_proj/ARD        # screensharingd x86_64 切片（已分析）
ard_re/decomp/                 # 反编译输出（.c 文件）
ard_re/disasm/                 # 反汇编输出（带符号标注）
```

### 客户端框架反汇编（Mac 端）

```bash
# Mac 上运行（需 ipsw 工具）
/opt/homebrew/bin/ipsw dyld disass \
  /System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_arm64e \
  --image ScreenSharing --force > /tmp/ss_disass.txt
```

---

*本文档由逆向工程产生，仅可用于互操作开发与防御性安全研究。*
*最后更新：2026-08-19*

---

## 附录 A：Rust 常量模块（直接可用于 src/vnc/session.rs）

```rust
//! ARD 会话层协议常量（逆向自 screensharingd macOS 26.6.1）
//! 详见 docs/ARD_SESSION_PROTOCOL.md

// ── ClientInit 能力位 ──
pub mod client_init {
    pub const SHARED: u8 = 0x01;
    /// bit6：客户端支持会话选择（SessionSelect）消息
    pub const SUPPORTS_SESSION_SELECT: u8 = 0x40;
    /// bit7：独占会话提示
    pub const EXCLUSIVE_HINT: u8 = 0x80;
    /// Apple 客户端标准值（shared | session_select | exclusive）
    pub const APPLE_CLIENT: u8 = 0xC1;
}

// ── SetEncryption 命令 ──
pub mod set_encryption {
    /// 消息类型字节（与 Apple 扩展 SetEncodings 共用入口）
    pub const MSG_TYPE: u8 = 0x12;
    pub const CMD_NEGOTIATE_ALGORITHMS: u16 = 1;
    pub const CMD_SET_DECRYPT_FLAG: u16 = 2;
    /// 唯一已知加密算法 ID
    pub const CIPHER_AES: u32 = 1;
}

// ── 换钥记录 ──
pub mod rekey {
    /// 换钥记录线长（[槽A 16B 新钥][槽B 16B 新IV]）
    pub const RECORD_LEN: usize = 32;
    pub const SLOT_KEY_OFF: usize = 0;
    pub const SLOT_IV_OFF: usize = 16;

    /// 初始密钥 = SHA256(SRP 会话密钥 K)[0..16]
    pub fn initial_key(srp_session_key: &[u8; 64]) -> [u8; 16] {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(srp_session_key);
        let mut key = [0u8; 16];
        key.copy_from_slice(&hash[..16]);
        key
    }
}

// ── EncryptOneMessage 帧 ──
pub mod enc_frame {
    pub const SHA1_LEN: usize = 20;
    pub const ORIG_LEN_FIELD: usize = 2;

    /// 明文块总长 = (原长 + 0x25) & !0xF（向上取整到 16B 边界）
    pub fn padded_len(orig_len: usize) -> usize {
        (orig_len + 0x25) & !0xF
    }
}

// ── 消息类型（state 4 分发器跳转表）──
pub mod msg_type {
    pub const SET_PIXEL_FORMAT: u8 = 0x00;
    pub const SET_ENCODINGS_RFB: u8 = 0x02;
    pub const FRAMEBUFFER_UPDATE_REQ: u8 = 0x03;
    pub const KEY_EVENT: u8 = 0x04;
    pub const POINTER_EVENT: u8 = 0x05;
    pub const CLIENT_CUT_TEXT: u8 = 0x06;
    /// Apple 扩展：SetEncodings 变体 / SetEncryption（通过子字段区分）
    pub const APPLE_ENCODING_OR_ENCRYPTION: u8 = 0x12;
    pub const SET_DISPLAY_CONFIG: u8 = 0x20;
    pub const SESSION_SELECT: u8 = 0x21;
    /// 单字节命令：请求设备信息/换钥记录
    pub const REQUEST_KEY_INFO: u8 = 0x64;
}

// ── Apple 私有编码值 ──
pub mod apple_encoding {
    pub const APPLE_PRIV_1002: i32 = 1002;
    pub const APPLE_PRIV_1011: i32 = 1011;
    pub const MVS_BASELINE: i32 = 1100;
    pub const MVS_ENHANCED: i32 = 1101;
    pub const DEVICE_INFO: i32 = 1103;
    pub const MVS_PROFILE_4: i32 = 1104;
    pub const APPLE_PRIV_1105: i32 = 1105;
    pub const APPLE_PRIV_1107: i32 = 1107;
    pub const APPLE_PRIV_1109: i32 = 1109;
    pub const APPLE_PRIV_1110: i32 = 1110;
    pub const CURSOR_PSEUDO: i32 = -239;
}
```

## 附录 B：逆向方法学（可复现流程）

| 步骤 | 工具 | 命令 |
|---|---|---|
| 函数反编译 | Ghidra headless | `analyzeHeadless ghidra_proj ARD -process screensharingd -postScript DumpAt.java <地址>` |
| 反汇编（带符号） | Ghidra headless | `... -postScript DumpDisasm.java <地址>` |
| 调用点查找 | Ghidra headless | `... -postScript FindCalls.java <函数名>` |
| 服务器日志 oracle | Mac 统一日志 | `log show --info --predicate 'process == "screensharingd"' --style compact` |
| 客户端会话捕获 | Mac MITM | `python3 /tmp/mitm36.py`，随后在 Screen Sharing 中连接 `vnc://127.0.0.1:15900` 并仅在系统提示框输入凭据 |
| 离线 SRP oracle | Python | 预设 `FRD_MAC_PASSWORD` 后运行 `python oracle2.py` |
| arm64e GOT 解引用 | Python | `GOT 值 & 0xFFFFFFFF + 0x180000000` |
| dyld 缓存反汇编 | ipsw (Mac) | `/opt/homebrew/bin/ipsw dyld disass <DSC> --image ScreenSharing --force` |

## 附录 C：关键 viewer 结构体偏移量速查

```rust
/// viewer 结构体（screensharingd）已逆向字段偏移
pub mod viewer_off {
    pub const STATE: usize = 0x1C;           // u32: 会话状态（枚举 1.2）
    pub const NETBUF_PLAINTEXT: usize = 0x08;  // *NetBuffer: 状态机数据源
    pub const NETBUF_CIPHER: usize = 0x9D0;    // *NetBuffer: 密文缓冲

    // 加密相关
    pub const DECRYPT_EVERYTHING: usize = 0x8F2;  // u8: cmd=2 flag
    pub const ENCRYPT_EVERYTHING: usize = 0x8F3;  // u8: 发送侧加密
    pub const REKEY_PENDING: usize = 0x8CA;       // u8: cmd=1 触发的换钥标志
    pub const REKEY_COUNTER: usize = 0x8CC;       // u32: 换钥计数器
    pub const ALGO_PARAM: usize = 0x8D0;          // u16: 算法参数
    pub const SLOT_A_KEY: usize = 0x8D2;          // [u8;16]: 当前密钥材料
    pub const SLOT_B_IV: usize = 0x8E2;           // [u8;16]: 当前 IV 材料

    // cryptor 指针（CCCryptorRef）
    pub const CRYPTOR_CBC_ENC: usize = 0x5F0;    // AES-CBC 加密
    pub const CRYPTOR_CBC_DEC: usize = 0x5E8;    // AES-CBC 解密
    pub const CRYPTOR_ECB_ENC: usize = 0x600;    // AES-ECB 加密（换钥槽加密用）

    // 会话选择
    pub const SESSION_SELECT_PREF: usize = 0xF75;  // u8: ClientInit 0x40 位结果
    pub const SESSION_SELECT_NEEDED: usize = 0xF76; // u8: state 3 vs 4 决定

    // 帧计数器
    pub const RECV_FRAME_COUNT: usize = 0x918;   // u32: 解密帧计数
    pub const SEND_CHUNK_COUNT: usize = 0x914;   // u32: 发送分块计数

    // 帧缓冲
    pub const FRAME_BUF_A: usize = 0x920;   // *u8: 解密后明文
    pub const FRAME_BUF_B: usize = 0x928;   // *u8: 密文读取
}
```


---

## 九、HPSS 高性能屏幕共享（2026-08-21，证据边界）

### 9.0 Apple wire 证据与 FreeRemoteDesk 本地策略

本章的 `0x1d`、严格媒体包络中的 `0x451 ServerState`、`0x09` 以及非增量
FramebufferUpdateRequest 字节，来自既有反汇编、捕获或有界实验，是 Apple wire
互操作证据。它们只证明消息形状、顺序或已观察到的服务端响应，不把任何本地超时、
发布时机或产品裁剪重新解释为 Apple 协议语义。

当前 `frd-protocol-apple` 的严格 High Performance 产品路径另有一组本地
fail-closed 策略：

- 产品工厂只接受现有命名的加密 `APPLE_SRP` 类型；legacy shared 认证仍仅供
  通用研究接口使用，不能进入产品 runtime；
- 成功写出既有 `0x1d` 后，必须在本地五秒期限内收到并严格解析首个
  `0x451 ServerState`，否则返回 typed
  `apple_high_performance_unavailable`；
- 确认前 generation、Reset、TransportReady、能力与音频状态均不公开；严格几何
  的新全量请求成功写出后，才激活首个公共 generation；
- 加密会话、HPSS 认证成功、`0x1d` 写入或收到任意 ServerState，任一项单独成立
  都不能证明 stock macOS 已接受 High Performance 虚拟显示，更不能替代实体显示器
  置黑/恢复和完整远程桌面的有界真机观察。

以上五秒期限、延迟 generation、typed failure 和 encrypted-only 选择均是
FreeRemoteDesk 产品安全策略，不是从 ARD 3.10 推导出的新 wire 字段或服务器保证。
现行设计见
[`Apple High Performance Session`](superpowers/specs/2026-08-29-apple-high-performance-session-design.md)。

### 9.1 协商链路（客户端 → 服务器）

| 消息 | 方向 | 格式 | 语义 |
|---|---|---|---|
| 0x1d | C→S | `[0x1d][u16 1][u8 0x30][u16 1][u16 1][u32 0][u8 1][u8 40][UTF-8 名 40B][零填]` 308B | 已捕获的 SetDisplayConfiguration 虚拟显示请求 |
| 0x451 | S→C | ServerState（1440×2560）| 已观察到的 post-`0x1d` 显示状态；严格解析可取得几何，但本身不证明产品模式或实体显示器置黑 |
| 0x08 | C→S | `[08 00][f64 BE scale][u16 zero reserved]` | SetServerScaling；`3fe6/3fed/3fee` 是浮点数高位，不是 subtype |
| 0x451 | S→C | 严格媒体矩形包络 + 声明长度 + ServerState 记录 | 显示状态族；不能据此推断 UDP 查询/应答关系 |
| 0x09 | C→S | `[09][00][u16 1][u32 0][u32 0][u16 w][u16 h]` | 已验证携带显示尺寸并启动 MVS 捕获路径；以新尺寸发送仍是 opt-in 实验 |
| 0x0d | C→S | `[0d][01][u32 0][u16 0]` | fence/同步 |
| 0x15 | C→S | `[15][00][u16 ver][u32 0]` ver=1/2 | AutoPasteboard |

### 9.2 媒体流（服务器 → 客户端，加密帧内）

```
矩形 = [u32 stream=1][u16 x][u16 y][u16 w][u16 h][s32 enc][数据]
enc:
  0x3f3 (1011)  MVS 视频记录（已知全量前缀与表初始化路径；部分更新另有状态化解码路径）
  0x450 (1104)  光标（[u32 0x3e8][u32 尺寸][zlib 数据]）
  0x451..0x456  会话状态族（ServerState/键盘/布局/机器）
```

### 9.3 已实现的保守 P1/P2 行为

P1 仅由 `hpssview --dynamic-resolution` 启用，默认关闭。运行时先收集
“匹配初始尺寸的 ServerState + 已成功应用的当前 generation 完整 MVS 全量帧 +
本地交互控制角色”三项证据；任一未知即不可用。窗口尺寸变更经 250 ms 防抖后，最多保留
一个在飞请求和一个最新候选。调整尺寸的 `0x09` 成功写出后才可见 `Pending`；只有精确
匹配的 ServerState 才能提交新的 generation。提交将原子替换显示 surface、重置 MVS
组装/解码状态并请求非增量全量帧；两秒内未确认则保留旧 surface。渲染和指针映射取当前
窗口与当前 surface 尺寸。

P2 先按声明总长度组装 MVS 记录，不把后续加密应用帧重新解释成媒体头。已捕获的
32748 + 26572 = 59320 片段是该规则的回归用例。全量、部分和畸形负载严格分类；畸形
负载不进入错误 decoder 路径，而是按当前 generation 请求全量重同步（200 ms 限制写入
速率，不丢弃唯一所需请求）。量化表、参考帧和等待全量状态均绑定显示 generation。

ARD 3.10 type-1 已按捕获证据实现 opcode 0/1/2/3、固定 Cb/Cr extent、`mvs` 终止、
cache/scan-order 引用和 generation 状态；严格 `FRDMVS02` 回放覆盖 18 条捕获记录，旧的
无版本/无几何捕获格式保持拒绝。该证据不覆盖所有非八对齐边缘、质量语义或长期网络条件。
调整尺寸的 `0x09`、本节新增的严格 High Performance 门禁、切换后的表下发和交互窗口仍
须分别完成真机验证；本文也不以现有证据推断 UDP、HDR、帧率语义或额外线格式字段。

### 9.4 客户端判定链（伪造服务器实测）
1. ServerInit 60B 必须逐字节克隆真实（时间戳区格式=Apple-ness 钥匙）
2. URL 带凭据启动 → "ssh tunnel" 误判 → 强制 compatibility（不发 0x1d）
   地址簿手动连接 → 完整 HPSS 协商
3. 周期心跳（2s）缺失 → 13 秒断开

### 9.5 P4 UDP 控制面与当前实现

服务器 `InitializeUDPVideoStream` 先发送 encoding `0x3f2` 的 54B
MediaStream Message 1：16B 媒体矩形包络、36B body、三个
`u16 port + u32 flags` 描述符和 10B 零保留区。客户端收到该消息后才发送
`0x1c` MediaStreamConfiguration。当前已确认 version 3 完整结构：会话 UUID、
各角色双向独立主材料和五个 binary-plist negotiator offer；wire 长度是
`messageSize + 4`，不是固定 96564B。服务器以 version 2 Message 2 返回各角色
压缩 answer，客户端严格校验长度、保留区和尾随数据后才激活数据面。

Rust 已将 Message 1 typed parser 接入 HPSS，并按 Audio/Video1/Video2、generation
绑定本地 UDP socket。所有 socket 在配置发送前完成 bind，异常或 teardown 同代关闭。
收到合法 Message 2 后进入 `Active`，使用 Apple 兼容的 AES-256-CTR、HMAC-SHA1-80、
独立 RTP/RTCP KDF、重放窗口和加密索引 SRTCP。真机有界运行已认证 audio/video RTP，
并将非静音 48 kHz 双声道 AAC-ELD 输出到 Windows 音频设备。

**P5 产品门控结论（2026-08-23）：** mode-4 有界实验曾得到认证、重放接受且位于
发送范围内的 SRTCP 报告。它证明通用 AVConference 端点接收/报告报文，但不证明
ARD 产品拥有该 password-HPSS 流，也不证明解码、播放或应用可读的远程输入设备。

stock macOS 26.6.2 `ScreenSharing.framework` 的离线 Ghidra 恢复闭合了产品门控：
`-[SSSessionView audioChatSupported]` 仅在 `idsSession` 非空或目标地址被判定为
Apple-ID 邀请时返回真；`setAudioChatMuted:` 只分流到已接受 QR/IDS 邀请的
AVConference、legacy IDS 或 invitation agent。ARD 3.10 主程序也没有恢复到 Audio Chat
控制路径。因此项目允许的用户名/密码 HPSS 登录模型不支持 P5 Audio Chat，通用麦克风
输入设备更没有 Apple 证据。`--udp-audio-input` 在网络会话选择前 fail-closed，Windows
麦克风不会打开；mode-4 发送器只保留用于离线协议回归。禁止用 Apple-ID、companion、
relay、proxy、daemon、driver 或 plug-in 绕过这一结论。完整证据见
`ard_re/P5_PROTOCOL_ANALYSIS.md`。
