# RDP 完整快照发布与 macOS 黑屏复查（2026-09-22）

## 现场与根因边界

macOS ARM64 旧本地 release 应用使用保存的凭据连接授权 Windows 后，
界面显示已连接，但只呈现顶部部分桌面，其余区域黑色。切换到 1280×720
重连后完整显示，控制岛隐藏时点击开始菜单、键盘输入和搜索画面更新正常。

本机主显示器物理分辨率为 2560×1440。从当前源码重新打包后，尚未加入
本次快照修复的对照会话也能在显示器原生模式完整显示并持续更新；帧指标
记录超过 2600 次成功批次且没有批次失败。因此，首次卡住尚不能唯一归因于
下面的代码缺陷，也不能声称该现场问题已稳定复现或彻底消除。

## 确定性缺陷与修复

`RdpBaseline::recover_full_snapshot` 原来将约 1MiB 的每个条带分别提交，
首条带就标记 `FullBaseline`，允许只有顶部像素的事务成为首个完整画面。
队列背压清空待提交条带后，启动恢复还会只重发当前条带，丢失快照前缀。

现在保留单个 patch 的字节上限，将整幅快照的 patches 放入同一个 Damage
revision，再提交唯一的 FullBaseline。Damage 或 boundary 遇到背压时，
以更高 revision 重发整个快照，最多重试一次；失败时不保留已建立基线标记。
不扩大 mailbox 预算，不改变分辨率选择、平台窗口、输入或解码器协商。

这属于共享 RDP 发布层修复，适用于 Windows/Linux/macOS 客户端；本轮
真实 GUI 仅覆盖 macOS ARM64，不能推断其他客户端的真机结果。

## 验证

新增完整覆盖与 boundary 清队列恢复测试，先确认旧实现失败，再验证修复。
14 项 baseline 测试通过；其中真实 MailboxSurfacePublisher、FrameMailbox、
FrameTransactionCompiler 链路验证 4×4 彩色图的全部像素，以及 3 项/64 字节
队列预填后溢出、清理、完整重发后的像素和 revision。
最终 RDP、frd-frame 和 macOS 应用测试合计 368 passed / 3 ignored；
`cargo fmt --all -- --check`、`git diff --check` 通过。

修复后 ARM64 release 应用重新 staging，Mach-O 架构、嵌套签名、包结构与
CLI verifier 通过。真实 GUI 使用保存的 Keychain 凭据、原生分辨率模式
重新连接；完整桌面持续刷新，至少 50 秒内记录 2402 个成功呈现批次，
没有批次失败。控制岛隐藏时点击开始菜单、键盘搜索均有可见响应，随后
退出搜索并保留连接。指标只含时序/计数，不包含桌面像素或输入文字。

本次有界复测未再次出现半屏停滞，不代表长期运行、其他显示器或所有
队列时序已验收；原始现场停滞与确定性快照缺陷的因果关系仍未唯一证明。
后续修复了审查指出的跨批次风险：消费者已经取走 Damage 并等待其
boundary 时，后续积压清队列可能删除该 boundary，下一快照触发
`RevisionWhilePending`。呈现端现在在邮箱锁内只提取已闭合的事务前缀，
尾部单个 Damage 留在邮箱计入预算，直到 boundary 到达。Reset 可以先提交；
原始 pop 清理语义、时间戳、严格非法序列拒绝和发布唤醒保持。
确定性测试同时覆盖启动与已连接状态：原始消费路径复现错误，新路径恢复成功。
另有未闭合字节/时间戳保留、完整前缀、Reset 与非法序列测试。

有界指标进程随后达到 32768 行诊断上限并退出，stderr 为
`frame_metrics_invalid_startup_state`，对应 `CapacityExceeded`；这不是解码失败。
后续应用复测关闭该诊断环境变量，避免把有界采集当作长期产品运行。

跨批次修复最终 `cargo test --locked --workspace --quiet` exit 0：78 组，
1782 passed / 0 failed / 16 ignored。`frd-frame` 37 项及 desktop shell 215 项
通过。ARM64 release 包重新生成并通过嵌套签名、结构及 CLI 验证。
新版无指标采集的 GUI 已打开，但本轮自动化点击最近连接未能载入配置，
因此停在登录页；此版本真实连接复测尚未通过，不能复用上一修订的 live 结果。

后续已定位并修复保存密码读取阻塞导致的登录页假死，见
[异步凭据加载](../superpowers/specs/2026-09-22-saved-credential-loading.md)。
包含完整快照、已闭合事务消费及凭据加载修复的新包，手动输入后成功登录，
原生分辨率完整桌面持续更新。本轮没有启用有界帧指标采集；键盘焦点测试
未通过，故不扩大为所有输入/长期稳定性已验收。
生产编码继续使用 LegacyOnly，本次不关闭 AVC、HEVC 或 Progressive 的独立门禁。
