# Windows 原生 RDP 离线验证记录（2026-08-29）

## 范围与结论

本记录只覆盖 Windows 客户端的离线测试、依赖边界审计和本机构建。它不构成
Windows Remote Desktop Services 的真机登录、首帧、键鼠、证书、NLA、剪贴板、音频或
Display Control 互操作证明。因此 README 中 Windows RDP 仍为 **开发中**。

已实现并纳入离线门禁的适配器边界包括：系统信任链和显式证书确认/精确 pin、仅
CredSSP/NLA 的连接路径、licensing/activation 基线、BGRX 脏矩形发布、fast-path
键鼠、单主显示器 Display Control、Unicode 文本 CLIPRDR，以及 48 kHz 双声道 PCM
RDPSND 发布。离线基线图形仅为 Raw、Interleaved RLE、RDP 6 Bitmap 和 RemoteFX。
EGFX、ZGFX、AVC/AVC420 与 AVC444 均未实现或验证，不得因本次构建而作出支持声明。
未纳入当前范围的其他能力仍为：RDPDR/文件/磁盘/设备、AUDIN/客户端麦克风、网关、
智能卡、打印机和多显示器。

## 工具链

所有 Rust 命令均使用显式 `+stable`。已验证的实际工具链为：

- `rustc +stable -Vv`: rustc 1.96.0 (ac68faa20 2026-05-25)，
  `x86_64-pc-windows-msvc`，LLVM 22.1.2。
- `cargo +stable -V`: cargo 1.96.0 (30a34c682 2026-05-25)。

未使用名为 `1.96.0` 的不完整目录，也未安装或卸载任何工具链。

## 命令与结果

下列命令在 `D:\FreeRemoteDesk\.worktrees\windows-rdp` 执行，均退出成功，除明确
列出的 11 项既有、带原因的忽略测试外没有失败：

```powershell
cargo +stable fmt -- --check
cargo +stable test -p frd-protocol-rdp
cargo +stable test -p frd-shell-desktop
cargo +stable test -p freeremotedesk-windows --test dependency_boundary
cargo +stable test --workspace
cargo +stable test --workspace --no-default-features
cargo +stable build -p freeremotedesk-windows --release
cargo +stable build --no-default-features
cargo +stable tree -p frd-protocol-rdp -e normal
rg -n "NoCertificateVerification|danger_accept_invalid_certs|SSLKEYLOGFILE|--password|ClearTextPassword" crates/frd-protocol-rdp apps/freeremotedesk-windows
git diff --check
```

| 门禁 | 结果 |
|---|---|
| 格式 | `cargo +stable fmt -- --check` 通过 |
| RDP 协议单元/文档测试 | 92 通过，0 失败，0 忽略 |
| 桌面 shell 单元/文档测试 | 40 通过，0 失败，0 忽略 |
| Windows 依赖边界集成测试 | 2 通过，0 失败，0 忽略 |
| 完整 workspace（默认特性） | 832 项列出；821 通过，0 失败，11 既有忽略 |
| 完整 workspace（`--no-default-features`） | 832 项列出；821 通过，0 失败，11 既有忽略 |
| 发布构建 | `freeremotedesk-windows` 通过 |
| 无默认特性构建 | 通过；见下方既有警告 |
| 差异空白检查 | `git diff --check` 通过 |

通过 `cargo +stable test --workspace [--no-default-features] -- --list` 复核两个配置均列出
832 项；源代码中的 11 个 `#[ignore]` 均为现有、需要未纳入公开仓库的授权媒体/捕获
fixture 的测试。因此实际执行的 821 项全部通过。

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
- 大小：41,569,280 bytes
- SHA-256：`17932F150A78E01A0AF32AEBF03442713D6EAE296682EDE5051437D5DBD63D7C`

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
| `qwinsta` | 当前用户 `rabbit` 的 `console` 会话 ID 1 为 `Active`；仅有 `rdp-tcp` 的 `Listen` 条目。 |
| 产品安全 profile/pin 存储（只统计，不输出地址、用户名或密码） | `connections-v1.json` 有 1 个有效 profile，但 `windows-rdp` profile 为 0；server-identity-pins 目录不存在，pin 文件为 0。未枚举或读取任何其他 Windows 凭据。 |
| Release 产物 | `target\release\freeremotedesk-windows.exe` 存在（41,569,280 bytes）；其哈希见上节的 Task 9 离线构建记录。 |

唯一已知且可监听的 RDP 目标正是运行 Codex 的当前主机。对该主机发起完整的回环
RDP 登录可能切换或锁定上述 Active console，从而中断当前工作会话；此外产品安全
profile 中没有另一台已授权的 RDP 目标和凭据。因此未运行证书确认、NLA、首帧、
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
