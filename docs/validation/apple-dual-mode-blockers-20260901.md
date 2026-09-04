# Apple Standard 与 High Performance 双模式阻塞记录（2026-09-01）

## 结论与范围

Windows FreeRemoteDesk 连接 stock macOS 的 Apple 路线必须按两种模式隔离：

| 模式 | 服务端语义 | 当前 FreeRemoteDesk 状态 | 本记录的结论 |
|---|---|---|---|
| Apple Standard | `displayType=0` compatibility；使用实体桌面，不创建 HP 虚拟显示 | **开发中**；adapter 尚未实现或注册 | 不能把现有 HPSS/MVS、认证或渲染子系统说成 Standard 已实现 |
| Apple High Performance | `displayType=1/2` virtual display | **开发中**；Windows 客户端已直连验证媒体协商与认证后的 UDP 视频，仍缺 HEVC 解码、surface 发布、持续输入及动态分辨率闭环 | 不能把媒体数据面子系统证据、type-1 置黑观察、隧道实验或 Standard 连接说成完整 HP 已验证 |

本文件只汇总截至 2026-09-01 的代码/运行记录与用户观察；不改变协议实现，也不把
日志中的 Apple 客户端行为扩展为 FreeRemoteDesk 互操作结论。

## Standard：当前的实现缺口

Apple Standard 是 `displayType=0` 的 compatibility 路线：画面来自实体桌面，且不发送
High Performance 虚拟显示配置。当前产品的 Apple adapter 尚未实现或注册该路线，因此
状态为 **开发中**。特别是，现有 High Performance 会话构造、HP 配置写入、MVS 解码和
wgpu 上传都不能替代 Standard 所需的独立选择/认证/经典 RFB codec-6 首帧与持续更新路径。

`target/validation/screensharing-current-desktop-system-log-20260901.txt` 记录了 Apple
Screen Sharing 的 current-desktop 观察，包括 `usingVirtualDisplay 0`、
`DRUnavailableInStandardConnection` 和 classic framebuffer update；它是 Apple 客户端
行为证据，不是当前 FreeRemoteDesk Standard adapter 的验证。

## High Performance：已知观察与仍然阻塞的验证

High Performance 对应 `displayType=1/2` virtual display。2026-09-01 的 type-1 运行中，
实体屏幕置黑是已完成的用户观察；这只能证明该 Apple 客户端实验发生过该可见效果，
不证明 Windows FreeRemoteDesk 已经建立、解码、持续刷新并可控制严格 HP 会话。

已知的客户端实验限制如下：

- 经隧道连接时，stock Apple 客户端会禁用 Pro；该路径不能充当 HP 成功证据。
- 在同一台 Mac 上直接运行 Apple 客户端时，服务端以“不能控制自己的屏幕”拒绝；该路径同样不能充当 HP 成功证据。

`target/validation/screensharing-high-performance-one-display-system-log-20260901.txt`
记录了 `usingVirtualDisplay 1`、virtual display 创建/状态转换和 classic framebuffer
update；它与上述 type-1 用户观察相互参照，但不替代 Windows FreeRemoteDesk 的直连
interoperability transcript。

### Windows 客户端直连媒体证据

2026-09-01，Windows FreeRemoteDesk 使用本地安全凭据提供器直接连接 stock Mac，完成
类型 36 认证、1920×1080 虚拟显示确认、`0x3f2` Message 1、1172-byte `0x1c`
configuration、Message 2 校验和 SRTP/SRTCP 激活。首次 8 秒有界运行收到 779 个通过
SRTP 认证并解密的视频 RTP 包，共 1,002,723 payload bytes；诊断证据位于 ignored
`target/validation/windows-direct-hp-video-20260901-094349/`。

`FRDVTP01` 离线解析确认该路视频只有 payload type 100，并同时出现 HEVC single NAL、
type-48 aggregation packet（含 VPS/SPS/PPS）和 type-49 fragmentation unit；分片中观察到
type 1 与 type 20 NAL。该结果证明 Windows→stock Mac 的 HP UDP 视频数据面可达，且足以
启动严格 HEVC RTP 重组实现；它仍不证明首帧已经解码/发布、画面持续刷新、输入闭环或
动态分辨率。

随后把产品视频 negotiator 从无产品调用证据的 mode 5 修正为 stock Screen Sharing 静态
确认的 mode 7，并把媒体 Active 后的 TCP 读取等待由 500 ms 改为 5 ms、每轮优先排空 UDP。
同一 stock Mac 的第二次 8 秒直连成功接受 mode 7，收到 10,712 个 PT100 视频 RTP 包，
共 14,417,629 payload bytes；证据位于 ignored
`target/validation/windows-direct-hp-mode7-20260901-095620/`。吞吐包数较首轮提高约 13.8 倍，
确认原轮询会饿死 UDP 接收；样本仍存在乱序和 sequence 缺段，因此下一门禁是扩大 socket
接收缓冲并实现有界 RTP 重排/AU 组装。任何缺片帧都必须整帧丢弃，不能拿残缺样本冒充
解码闭环。

第三次 8 秒直连为每个媒体 socket 请求并实际取得 4 MiB `SO_RCVBUF`，收到 14,490 个
PT100 视频包、19,494,459 payload bytes；证据位于 ignored
`target/validation/windows-direct-hp-buffer4m-20260901-101759/`。按首包模 16-bit 序列区间
计算，仅缺 46 个唯一序号，缺失率约 0.32%，250 个 timestamp 均观察到 marker；transport
同时报告 0 个 malformed、0 个 authentication failure，以及 121 个 replay-or-too-old。
第四次 8 秒直连把 RTP anti-replay 窗口与 256 包视频重排窗口对齐；证据位于 ignored
`target/validation/windows-direct-hp-replay256-20260901-102301/`。本轮接受 14,510 个
PT100 视频包，覆盖 14,530 个模 16-bit 序号，仅缺 20 个唯一序号（约 0.14%）；255 个
timestamp 均观察到 marker，且 authentication failure 仍为 0。transport 另报告 117 个
replay-or-too-old，这些计数包含重复包或迟到超过窗口的包，不能直接等同于序号缺失。
因此 socket 饿死、接收容量和本地 anti-replay 窗口已经实质收敛；下一门禁是把已实现的
重排/AU 组装接入 Main444 解码后端，并在真实丢片时确认整 AU 丢弃与关键帧恢复。
严格 `AppleUdpMedia` encoding profile 现仅供这一独立诊断链使用；在 Main444 解码、
surface 发布和首真帧门禁完成前，不把产品 factory 全局切到该 profile，避免认证成功后
丢弃视频 payload 并以黑屏进入 Ready。此暂缓不是回退或自动降级，而是未完成模式不注册。

同一 mode-7 样本中的 SPS 已离线解析为 `general_profile_idc=4`、
`chroma_format_idc=3`、8-bit、coded 1920×1088 且底部裁剪 8 行，即实际 1920×1080
HEVC Main 4:4:4。它与 Apple 静态 Main444 证据一致，并排除了把 Windows 系统 HEVC
Main/Main10 4:2:0 解码器直接当作通用 HP 后端的做法。Windows 解码后端必须显式声明
Main444 能力；不支持时报告能力错误，不能静默切换 Standard。

## 第二个 macOS 客户端实例的边界

缺少第二个 macOS 客户端实例，只阻塞使用 ARD/Screen Sharing 客户端取得精确 AVC、UDP
和动态分辨率 runtime transcript。它**不阻塞** Windows FreeRemoteDesk 主动直连当前授权
stock Mac（地址仅来自 `CREDENTIALS.local.md`/安全提供器）并进行有界互操作验证；上述
Windows 直连已经证明 HP UDP 视频数据面可达，后续继续沿同一路径完成解包、硬解与发布。

`target/validation/screensharing-high-performance-direct-system-log-20260901.txt` 与
`target/validation/screensharing-high-performance-dynamic-resolution-system-log-20260901.txt`
保留了 2026-09-01 的相关 Apple 客户端运行记录。它们用于界定缺失 transcript，不能被
解读为已取得所需的第二客户端精确媒体/动态分辨率证据。

## 后续 High Performance 验证路线

1. 继续以 ARD 3.10 静态恢复约束字段、状态机和解码假设。
2. 用 Windows FreeRemoteDesk 直连 stock Mac，分别验证 `0x3f2`/`0x1c`、UDP、HEVC 首帧与动态 resize。
3. 将每项直连结果与模式选择、首帧、持续刷新、输入门禁和断开恢复关联；失败保持 **开发中**，不以相邻模式或离线结果补足。

## 不可跨越的产品边界

- 不允许使用隧道、把 Standard 降级连接，或其他替代路径冒充 High Performance。
- 不请求、不保存、不使用 Apple ID、iCloud、IDS、APNs 或 QuickRelay 身份凭据。
- 不安装或运行服务端助手、守护进程、代理、虚拟显示/音频驱动或其他 Mac 端配套组件。
