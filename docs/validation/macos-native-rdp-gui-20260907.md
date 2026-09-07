# macOS 原生 GUI 连接 Windows RDP 验证记录

**日期：** 2026-09-07
**客户端：** `target/macos/debug/FreeRemoteDesk.app`（arm64，staged，ad-hoc signed）
**目标：** 经授权的 Windows Remote Desktop Services（3389/TCP）

本记录只描述验证范围，不保存密码、命令行凭据、会话密钥或抓包材料。密码由
macOS GUI 的安全输入框输入；启动参数、普通配置、日志和截图均未包含密码。

## 实机结果

1. 从 macOS 原生登录窗口填写 Windows 目标、端口、用户名和密码，并勾选“在此设备上保存登录信息”。
2. RDP TLS/CredSSP/NLA 完成认证和 activation。窗口状态显示“已连接”，并显示服务器证书主体、签发者和 SHA-256 指纹：
   `57:D7:12:09:D2:F1:8F:DE:9E:A9:98:31:1B:C7:59:D3:FE:36:5B:22:B7:F3:1C:73:FC:95:CF:F6:39:C5:10:D8`。
3. 远程内容区显示实际 Windows 桌面和 PowerShell 窗口；没有停留在黑屏或仅显示连接状态。
4. 断开后，登录窗口的“最近连接”出现该 Windows 目标的保存记录。选择该记录后，密码框自动恢复为隐藏字符、保存复选框恢复为选中状态，无需再次输入密码。
5. 使用自动恢复的密码再次连接，窗口再次显示同一台 Windows 桌面。

## 凭据和配置边界

- macOS profile store 只保存协议、地址、端口、用户名、目标系统和最近成功顺序等非敏感元数据。
- 密码使用 Keychain service `org.freeremotedesk.credentials.v1`，不会写入 profile JSON、日志或进程参数。
- profile 选择、Keychain 读取和 TransportReady 后的提交均在后台 worker 中执行；系统钥匙串锁定或授权失败不会阻塞窗口，而是让密码框保持可编辑并提示重新输入。
- Windows 客户端仍使用自己的 Windows Credential Manager 分支；macOS Keychain 代码位于独立 `frd-platform-macos` crate，平台条件编译没有改变 Windows store 接口。

## 黑屏修复

IronRDP 可能先发送多个局部 bitmap 更新。旧逻辑在尚未覆盖完整桌面时就把局部边界
发布为首个完整 baseline，frame transaction compiler 因不满足完整性而拒绝，表现为
连接状态已成功但内容区黑屏。本轮改为先累积 startup coverage；只有精确覆盖整个
surface 后才发布首个完整 snapshot。之后的恢复 snapshot 仍按有界 patch 发布，最后
一个边界标记为完整 baseline。`frd-protocol-rdp` 的 baseline 回归测试覆盖了局部先行、
精确覆盖和恢复边界。

## 自动化验证

在 macOS arm64 主机使用 Rust 1.96 运行：

```text
cargo fmt --all -- --check
cargo test -p frd-app --quiet                 # 80 passed
cargo test -p frd-shell-desktop --lib --quiet # 206 passed
cargo test -p frd-protocol-rdp --quiet          # 116 passed
cargo test -p frd-protocol-rdp baseline_ --quiet # 8 passed
cargo test -p frd-platform-macos --all-targets --quiet # 24 passed, 2 ignored
cargo test -p frd-platform-macos --all-targets -- --include-ignored secure_credentials --nocapture # 4 passed
cargo check -p frd-app -p frd-shell-desktop -p freeremotedesk-macos
tools/stage-macos-package.sh debug
```

Windows 目标的 `frd-app` 跨目标检查通过；在本 macOS 主机直接对完整 Windows 目标
运行 `cargo check --target x86_64-pc-windows-msvc` 仍需要 Windows SDK/MSVC C 工具链，
因此该命令的失败属于主机工具链限制，不是 macOS 分支改变 Windows API 的证据。

## 未覆盖范围

本轮只对一台授权 Windows 目标做有界登录、证书、首帧、显示、断开和保存凭据验证。
没有宣称多目标长期稳定性、完整键鼠/焦点行为、剪贴板、音频、动态分辨率、证书换证
实机操作或公证发布；这些仍按平台矩阵保持独立状态。
