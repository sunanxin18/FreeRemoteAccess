# ARD P3/P4/P6 媒体可靠性与协议语义化设计

## 状态与范围

本设计以已批准的“方案 A”为基础：协议符号按所有权分散在各自模块，
另用一份跨模块 wire symbol 文档建立可检索索引。它替代旧设计中已经被
现场证据推翻的 UDP 能力查询、固定长度 `0x1c` 和 HPSS 双向音频假设。

本次实现范围包括：

- P3 Mac→PC 音频在丢包、乱序、设备或解码器故障下的可靠性与隔离；
- P4 UDP/SRTP/SRTCP 接收的公平性、抗噪声能力和可观测性；
- P2 审计发现的 MVS 表初始化严格性缺口；
- P6 RFB/Apple 会话解析、资源预算、整数转换、线程生命周期、依赖审计、
  协议魔数和文档一致性修复。

P5 不在本设计的生产实现范围内。现有证据表明 Apple 原生 PC→Mac 音频属于
IDS QuickRelay/AVConference AudioChat，而密码认证 HPSS 的 `SSUDPSender` 是
发送路径。选择“重实现 Apple 私有身份/中继栈”还是“部署 Mac 伴随端”会产生
完全不同的系统边界，必须单独批准。当前 `--udp-audio-input` 继续 fail-closed。

P1 动态分辨率状态机不改语义；所有共享几何校验必须保持其 generation 原子
提交约束。

## 设计原则

### 证据分级

每个 Apple 私有 wire 字段在代码和文档中标记为：

1. `Verified`：由静态生产者/消费者、已净化精确 fixture 或受控现场差分证明；
2. `Candidate`：已观察到稳定位置或行为，但语义名仍不完整；
3. `Blocked`：缺少可信布局或消费证据，禁止据猜测生成生产报文。

RFB RFC 字段引用规范；Apple 私有字段引用逆向证据文件或净化 fixture。
测试通过不升级为现场互操作证明。

### 数值与查表策略

生产协议值、消息类型、偏移、长度、标志/掩码、预算和超时必须使用命名常量、
枚举、新类型或 typed builder/parser。跨模块共享的同一 wire 值只能有一个所有者。

以下值可以保留原始数字，但必须放在命名对象中并注明来源：

- JPEG Annex K 等标准查找表；
- X11 `Key -> keysym` 映射；
- 与生产常量独立的 byte-exact 测试 fixture；
- 数学算法内部、与 wire 无关且名称不会增加语义的局部常数。

不建立全局 `wire_symbols.rs`。它会让 RFB、HPSS、MVS 和媒体加密互相耦合，
并造成“常量集中但所有权消失”。

## 模块所有权

| 所有者 | 负责内容 | 允许向外暴露的接口 |
|---|---|---|
| `protocol.rs` | 标准 RFB 消息、安全类型、资源上限 | RFB typed builders、security helper、limits |
| `session.rs` | Apple 加密会话命令和会话协商表 | typed command records/builders |
| `hpss.rs` | HPSS 控制消息、显示配置、MVS 外层记录 | typed control parser/builder |
| `media_protocol.rs` | Apple MediaStream envelope 与共享 wire ID | message records、长度/版本/kind 常量 |
| `media_negotiation.rs` | 能力到媒体配置的策略映射 | 使用 `media_protocol` 的共享符号，不复制 wire 值 |
| `media_transport.rs` | UDP socket、role、公平预算、SRTP/SRTCP 边界 | typed receive outcome、discard reason、per-role budget |
| `srtp.rs` | RTP/SRTP 序号、replay、ROC、RTCP 字段 | 序号分类和命名 bit-field helper |
| `audio_codec.rs` | AAC-ELD/RFC 3640/RTP audio 解码顺序 | typed audio receive outcome |
| `mvs.rs` / `mvs_stream.rs` | MVS 初始化、bitstream 和分片重组 | exact classifier、generation-bound decoder state |
| viewer 公共几何模块 | scale、窗口像素预算、输入映射 | `ValidatedScale`、checked viewport geometry |

跨模块索引写入 `docs/ARD_WIRE_SYMBOLS.md`，列出代码符号、wire 值/布局、
方向、证据等级和证据来源。该文档是索引，不是代码常量的第二份定义。

## P3 音频接收与故障隔离

### 序号分类

SRTP 层继续负责认证、ROC 推断和 replay window；通过认证且在窗口内的乱序包
可以到达音频层。音频层以 16 位序号半空间规则分类：

- 首包：建立 `last_forward_sequence`；
- 向前：距离在 `1..=0x7fff`，按时间戳对齐的缺口执行现有 concealment，然后
  推进前向序号；
- 重复或迟到：距离为零或落在反向半空间，返回 `DiscardedLate`，不调用
  AAC 解码器、不插入 concealment、不改变前向状态；
- payload type、RTP 结构或时间戳合同错误仍是语义错误。

`decode_rtp_packet` 改为返回可观测的 typed outcome，而不是用 `Err` 表达合法
乱序。viewer 分别统计 decoded、concealed 和 late-discarded 数量。

本批次不引入完整 jitter buffer。它会改变时延、队列上限和播放时钟，且当前
问题只需要保证已认证迟到包不会终止会话。后续若现场数据证明需要重排播放，
再用独立设计引入有界 jitter buffer。

### 音频子系统状态

`ViewerMediaState` 内的音频输出改为显式状态：

```text
Disabled -> Starting -> Active
                    -> Degraded(reason)
```

打开默认播放设备、AAC 解码、PCM 入队失败，只把当前 generation 的音频转为
`Degraded`，释放播放/解码资源并记录一次有界诊断；屏幕、控制和 UDP 视频继续。
新的 generation 可以重新尝试音频。媒体控制状态违例、TCP 会话错误和显示状态
错误仍可终止整个 viewer。

## P4 UDP 接收

### Typed receive outcome

`MediaTransport::try_recv_decrypted` 返回三类结果：

```text
Empty
Accepted(MediaDatagram)
Discarded(MediaDiscardReason)
```

以下来自不可信网络的数据报只丢弃并计数：

- 来源不是协商端点；
- 空包、过短包或无法区分 RTP/RTCP；
- SRTP/SRTCP 认证失败；
- replay/duplicate；
- 已认证但媒体负载结构无效。

以下错误保持 fatal：socket 非 `WouldBlock` I/O 错误、generation/role/phase 状态
违例、缺少已协商加密状态、内部计数器或长度合同被破坏。discard 日志采用首次和
幂次采样，避免攻击者制造日志洪泛；总计数使用饱和加法。

### 公平排空

`MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL` 由 `media_transport.rs` 唯一定义。
headless 和 viewer 每轮对每个 active role 分别应用该预算，并单独维护总处理数。
Audio backlog 不能消耗 VideoStream1/2 的配额。`Discarded` 也消耗本 role 配额，
避免垃圾流量让单轮无限循环。

role 顺序保持确定性；下一轮从轮转起点开始，防止长期高负载时固定首 role 获得
更低延迟。

## MVS 严格初始化

MVS table initialization 必须满足：

- payload 长度恰好为命名常量 `MVS_TABLE_INITIALIZATION_BYTES = 129`；
- rectangle 的 `x/y/width/height` 全为零；
- 前 64 字节为 luminance table，随后 64 字节为 chrominance table；
- 第 129 字节以 `initialization_parameter` 保存并标为 `Candidate`，在语义被
  证明前不参与推断；
- 128、130 字节、非零 rectangle 或 generation 不匹配都拒绝并触发现有有界
  full resync，不进入 JPEG 路径。

完整帧、partial 和 table record 的分类先于解码，分类器保持纯函数，便于使用
独立 fixture 验证。type-1 partial 字段仍为 `Blocked`。

## P6 解析、资源与生命周期修复

### RFB 与 Apple 认证

- RFB 3.3 的 32 位 security type 必须 `try_from` 为 `u8`；超出范围直接拒绝，
  不允许截断成另一个认证类型。
- 增加 `APPLE_ARD_39` 语义常量；安全类型名称和 `pick_security` 只引用常量。
- `requires_apple_account_credentials` 统一处理 Apple 30/33/35/36 的凭据提示。
- `SetEncodings` 数量使用 checked conversion，builder 返回 `Result`。
- SRP `%s/%o/%m` 和外层 TLV 长度全部 checked conversion；过长用户名在写 socket
  前失败。RSA-SRP 复用同一 checked builder。

### FFI、几何和 deadline

- AAC encoder 的 `maxOutBufBytes` 必须严格大于零并 checked-convert 为 `usize`；
  负数或平台溢出在分配前失败。
- CLI scale 由 `ValidatedScale` 校验：finite、严格大于零；换算宽高使用 checked
  浮点到整数边界和 `MAX_FRAMEBUFFER_PIXELS`。classic viewer 与 HPSS viewer 使用
  同一 helper，禁止各自 cast/saturating allocation。
- 用户提供的 `seconds`/`wait_ms` 用 `Instant::checked_add` 建 deadline；溢出返回
  中文错误，不 panic。固定内部短超时保留命名常量。
- HPSS display name 按 UTF-8 char boundary 截断到命名 wire capacity，剩余字段补零；
  整条 SetDisplayConfiguration 长度保持精确。

### 内存、锁和线程

- `Framebuffer::copy_rect` 改为经边界裁剪的重叠安全逐行 `copy_within`：目标位于
  源下方时自底向上，否则自顶向下；不再分配 `w*h` 临时缓冲。
- 用户可触达的网络、viewer 和回调路径不使用 `Mutex::lock().unwrap()`。
  可返回 `Result` 的路径把 poison 转为带上下文错误；不能返回错误的回调将子系统
  转为 degraded/closing 或丢弃该次回调。测试内部的 `unwrap` 不纳入此禁令。
- classic viewer 与 HPSS viewer 一样持有 reader handle；退出时设置 closing、
  shutdown socket 解除阻塞、join reader。正常关闭和 reader panic 都有确定结果。

### 依赖告警

依赖处理以“调用路径 + 目标平台 + 可用修复”为准：

- 对仅出现在非 Windows target graph 的 unmaintained transitive crate 记录风险并
  优先通过兼容的直接依赖升级移除；不得把非活动 target 误报为当前可利用漏洞。
- `rsa` 告警涉及私钥操作；生产客户端只使用公钥加密，私钥/解密仅在 mock test。
  在上游没有稳定修复时记录此边界，不伪造“已修复”。若未来引入生产私钥操作，
  该变更必须 fail CI 或先迁移实现。
- 每次依赖变更后运行两套 feature tree、测试、build 和 Clippy；不能仅凭版本号
  宣称安全闭环。

## 语义符号迁移清单

下面的清单由当前生产源码逐文件扫描得到。它区分“已有命名但所有权重复”和
“仍在 builder/parser 内直接写 raw value”，避免把已经命名的预算或标准表误报为
魔数。

| 当前模块/位置 | 当前问题 | 目标所有者与表示 |
|---|---|---|
| `protocol.rs` 的消息 builders | Client message 0/2/3/4/5 和 padding/count 直接写入 | `RfbClientMessageType`、命名 header 长度和 checked `SetEncodings` builder |
| `protocol.rs::security_type_name` | 0/1/2/16/17/19/22/30/33/35/36 与已存在 security 常量分离 | `SecurityType`/security 常量单一表；补 `APPLE_ARD_39` |
| `client.rs::read_server_message` | Server message type、Apple cursor discriminator 和部分长度用局部数值判断 | `RfbServerMessageType`、cursor/header typed parser |
| `session.rs` | SelectSession、SetEncryption 和三套 encoding 协商使用大块 raw byte arrays | `AppleSessionCommand`、`EncryptionMethod`、typed session builder；encoding list 由命名符号数组序列化 |
| `rsa_srp.rs` | RSA1 marker、版本、选择类型、帧长、响应 tag 和公钥上下限直接写入 | `RsaSrpFrameVersion`、`RSA1_MAGIC`、字段长度/响应 tag/资源上限常量；security type 引用 `protocol` |
| `srp.rs` | success tag、TLV 字段宽度和外层命令仍有直接数值 | checked `SrpTlvItem`/`SrpFrame` builder 与命名 response tag |
| `hpss.rs` display builders | `0x1d`、`0x30`、16B `0x09` 模板和固定字段直接写入 | `HpssClientMessageType`、`SetDisplayConfiguration`、`DisplayQuery` typed builder |
| `hpss.rs` control table | `0x03/0x08/0x0d/0x14/0x15` 与 `protocol` 的 RFB/keepalive 定义重复 | 标准 RFB 类型引用 `protocol`；Apple HPSS 扩展保留在 `hpss` |
| `hpss.rs` / `hpss_viewer.rs` | 同值 256 的 UDP drain 配额分别定义且语义不同步 | `media_transport::MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL` 单一定义 |
| `media_negotiation.rs` | primary id、`0x3f2`、answer version/kind 与 `media_protocol.rs` 重复 | 全部由 `media_protocol` 导出；negotiation 只表达策略 |
| `media_negotiation.rs` / `srtp.rs` | master key/salt 长度重复 | SRTP profile/key-material 类型由 `srtp` 所有，negotiation 引用该合同 |
| `media_transport.rs` / `srtp.rs` | RTP version/header/marker 和 RTP-vs-RTCP 区间各自定义 | 抽出 `RtpPacketKind`/header classifier，由 `srtp` 或专用 `rtp` 子模块单一所有 |
| `mvs.rs::parse_mvs_payload` | full/partial 三字节 signature 直接比较 | `MVS_FULL_FRAME_SIGNATURE`、`MVS_PARTIAL_FRAME_SIGNATURE` 与 typed payload kind |
| `mvs.rs::wrap_as_jpeg` | JPEG marker、component id、sampling selector 和 scan 参数散落在 push/array 中 | JPEG 标准 marker/component 常量和小型 typed segment writers；Annex K 表保留命名数据表 |
| `srtp.rs` reception report | 24 位 cumulative-loss sign bit/sign extension mask 直接写入 | `RTCP_CUMULATIVE_LOSS_SIGN_BIT`、`RTCP_CUMULATIVE_LOSS_SIGN_EXTENSION_MASK` 和 helper |
| `audio_codec.rs` | codec 参数多数已命名，但序号半空间分类尚未形成语义 API | `RtpSequenceDisposition` 和 `AudioReceiveOutcome` |
| `main.rs` / viewer | 连接/读超时、默认端口、窗口策略和 deadline 在调用点直接构造 | 命名 CLI/runtime policy 常量；用户值经 checked helper |
| `arp.rs` | 默认 CIDR、线程上限、banner 长度/超时和路由探测端点散落 | `ArpScanPolicy`/命名常量；RFB banner 长度引用 `client` 公共合同 |
| `framebuffer.rs` | RGBA channel shift/alpha 与 CopyRect 临时分配策略内联 | 命名 pixel layout；CopyRect 使用无整块分配的 checked helper |

`keysym.rs` 的映射表、`arp.rs` 的命名 `APPLE_OUIS` 数据集、`mvs.rs` 的命名 JPEG
Annex K tables 以及测试中的独立 expected byte fixtures 都属于允许的查表/证据例外。
这些例外不能被生产 builder 反向引用来生成“测试期望”。

实施结束时执行第二次同范围源码扫描。对每个仍留在生产路径的 raw literal，必须在
review 记录中归入以下一种：命名表数据、独立算法常数、显然的零/一结构操作，或
待迁移缺陷；最后一类必须为零。该分类结果同步进 `docs/ARD_WIRE_SYMBOLS.md`，并在
`AGENTS.md` 固化审查规则。

## RED→GREEN 测试矩阵

实现严格按下列顺序，每项先提交能在旧实现上失败的行为测试：

1. 音频序号 `100 -> 102 -> 101 -> 103`：102 只 conceal 一个 access unit，101
   被丢弃且不推进状态，103 正常解码且不产生额外缺口。
2. 用真实 loopback UDP socket 注入错误来源、空/短包、认证失败、replay 后，再发
   合法 SRTP；错误包被计数且合法包仍被接收。
3. 多 role 真实 socket backlog：每个 role 每轮都获得独立预算，discard 受预算约束。
4. 音频设备/decoder 注入失败后，display event 仍继续处理；下一 generation 可重试。
5. MVS 128/130 字节和非零 table rectangle 失败，129 字节准确解析并保存尾参数。
6. RFB 3.3 security type `257` 被拒绝而不是选择 `1`；Apple 30/33/35/36 凭据提示
   一致。
7. AAC FFI 负输出长度在 allocation 前失败。
8. scale 的 NaN、正无穷、零、负数和像素预算溢出失败；正常 scale 在两 viewer
   获得相同尺寸。
9. 极大 deadline 参数返回错误，不 panic。
10. 39 字节 ASCII 加多字节字符的 display name 不产生破损 UTF-8，wire 总长不变。
11. SRP 超过 `u8/u16/u32` 字段容量时 builder 失败；边界长度编码正确。
12. CopyRect 水平/垂直重叠、裁剪和大区域操作与快照 oracle 结果一致且无整块临时
    分配。
13. reader 正常关闭、远端关闭和线程 panic 路径都完成 join。
14. wire builder/parser 的 fixture 不引用生产常量构造 expected bytes，避免同错同过。

## 实现批次与回滚边界

每批只跨越一个可独立验证的合同：

1. P3/P4 receive outcome、乱序和 per-role 公平性；
2. 音频 degraded 状态和 viewer 错误隔离；
3. MVS exact table classifier；
4. RFB/security/SRP/FFI checked conversion；
5. scale/deadline/display-name/resource helper；
6. CopyRect、锁 poison 和 reader 生命周期；
7. 全仓语义符号迁移与 `ARD_WIRE_SYMBOLS.md`；
8. 依赖更新（只有存在兼容、可验证路径时）；
9. 文档与现场验证。

每批 GREEN 后跑 focused tests；跨模块批次再跑 default/no-default。不得为了让旧 fixture
继续通过而放宽 parser，也不得把 P5 的未批准数据面塞进 P3/P4 批次。

## 验收门槛

### 自动化

- `cargo fmt -- --check`
- `cargo test`
- `cargo test --no-default-features`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo clippy --all-targets --no-default-features -- -D warnings`
- `cargo build --all-features`
- `cargo build --no-default-features`
- 顶层与 `hpssview` help 输出
- wire symbol 文档与代码符号一致性检查

### 现场验证

- Mac→PC 非静音 AAC-ELD 连续播放，人工制造的短时丢包/乱序不终止画面；
- 错来源和认证失败 UDP 噪声不终止会话，合法音频/视频继续；
- 长于一个 RTP 序号周期的 audio/video 运行，验证 ROC、replay、loss 和 teardown；
- 音频输出设备不可用时屏幕和控制继续；恢复/重连后音频可重新初始化；
- P1/P2 现有 live 行为无回归。

现场验证必须记录环境、事件顺序和净化统计；自动化通过不能替代上述证明。

## 自审结论

- 设计没有把 MVS、AVC MediaStream 和 IDS AudioChat 的证据混用。
- 网络噪声、音频子系统失败、媒体状态违例三种错误边界已分开。
- per-role 预算同时覆盖 accepted 与 discarded，避免公平性修复产生 DoS 旁路。
- exact MVS 129B 合同保留未知尾参数，没有猜测其语义。
- P5 保持 fail-closed，等待独立架构选择。
- 所有生产 wire 数值有所有者；标准表、keysym 与独立 fixture 的例外明确且可审计。
- 验收同时覆盖自动化、依赖、线程 teardown 和现场长序号运行，没有把当前 136 项
  基线测试误当成新设计完成证明。
