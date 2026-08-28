# Windows 安全登录验证记录

**日期：** 2026-08-28

**范围：** Windows 产品组合、登录界面、本机安全存储；未读取
`CREDENTIALS.local.md`，未使用真实凭据。

## 构建产物

- 文件：`target/release/freeremotedesk-windows.exe`
- SHA-256：`B3D1090B88ED65276156CDDB390E4067A692241082C26BB7BB69FC659705A6ED`
- 构建命令：`cargo +stable build --release -p freeremotedesk-windows`
- 结果：成功，release profile 完成，无编译警告。
- 说明：该 SHA-256 对应本次实际启动的共享工作树产物；工作树还含已有、未暂存的标题栏/图标改动。另以 Git 暂存区精确快照独立验证本任务提交内容。

## 自动化验证

| 命令 | 结果 | 证据摘要 |
|---|---|---|
| `cargo +stable fmt -- --check` | 通过 | 最终重跑退出码 0；首次预检仅发现本任务新增代码的 rustfmt 布局差异，修正后从矩阵起点重跑。 |
| `cargo +stable test -p frd-platform-api` | 通过 | 2 个单元测试和 1 个 compile-fail 文档测试通过。 |
| `cargo +stable test -p frd-platform-windows -- --test-threads=1` | 通过 | 18 个测试通过；包含进程唯一 Windows Credential Manager 暂存、提交、读取、丢弃、删除及清理保护。 |
| `cargo +stable test -p frd-ui-model` | 通过 | 7 个测试通过。 |
| `cargo +stable test -p frd-app` | 通过 | 63 个测试通过；覆盖暂存/提交/回滚、成功后取消保存和单次连接意图。 |
| `cargo +stable test -p frd-ui-egui` | 通过 | 19 个测试通过；覆盖窄屏布局、图标帮助文本、密码 Enter 单次提交、IME 与重复键过滤。 |
| `cargo +stable test -p freeremotedesk-windows` | 通过 | 当前共享工作树为 12 个二进制单元测试、2 个依赖边界测试、2 个图标资源测试；精确暂存区快照为 11 个二进制单元测试和 2 个依赖边界测试，均通过。 |
| `cargo +stable test --workspace` | 通过 | 工作区测试及文档测试退出码 0；需要未公开授权 fixture 的既有测试保持 ignored。 |
| `cargo +stable clippy --workspace --all-targets -- -D warnings` | **阻断** | 既有已提交文件 `crates/frd-frame/src/surface.rs:35` 触发 `clippy::len_without_is_empty`；该文件不属于本功能且工作树中未修改，按任务边界未扩展修复。 |
| `cargo +stable build --release -p freeremotedesk-windows` | 通过 | 退出码 0，耗时 47.17 秒。 |

补充 RED/GREEN：新增启动清理测试最初因缺少
`purge_pending_credentials` 和 `RunnerFailure::CredentialStore` 编译失败；实现后两个精确测试均为 1 passed。依赖边界扩展在实现前已通过，因为 Task 4 已经完成 Windows 三类 store 的产品组合，本任务没有重复该接线。

为隔离共享工作树中的未暂存改动，另用 `git checkout-index` 导出精确暂存区快照；该快照的 `cargo +stable fmt -- --check` 和 `cargo +stable test -p freeremotedesk-windows` 均退出 0。

## 本机 Windows 验证

### 已观察

- 无命令行凭据启动 release 可执行文件，得到一个 `FreeRemoteDesk` 产品窗口。
- 当前系统浅色主题下，460 逻辑点上限的居中登录卡完整可见，无裁切。
- 可访问性树包含最近连接、目标系统、连接协议、地址、端口、用户名、密码、保存登录信息和全宽连接按钮；密码图标的可访问名称为“显示密码”。
- 没有可供本次隔离验证使用的合成最近连接，因此选择器按预期禁用；未读取或选择用户已有配置。
- Windows 平台测试以进程唯一目标执行真实凭据库往返并由清理保护删除测试条目；精确 `FreeRemoteDesk/pending/` 前缀筛选测试通过。
- 元数据 round-trip 测试只接受版本 1 非敏感 schema 键；凭据字节仅进入 Windows Credential Manager 测试路径，不进入元数据记录。

### 本次未运行

- 未切换系统到深色主题，因此没有本次人工深色视觉证据。
- 为避免覆盖并发的用户界面输入，本次没有继续执行悬停 tooltip、人工 Enter 提交或失败连接探测；这些行为仅有上述自动化测试证据。
- 未使用授权 Mac 完成 GUI 连接，所以没有产品级“成功连接后保存凭据”和“取消保存后成功重连并删除配置/凭据”的端到端证据。
- 未运行 Mac 实时互操作；不得由本机单元测试、Windows 凭据库测试或界面启动推断。

## 结论与状态

README 平台矩阵标记为 **开发中**：Windows 自动化状态机、本机真实凭据库原语和浅色登录页启动均有证据，但该矩阵要求原生服务端有界真机互操作；授权 Mac GUI 的 TransportReady 提交与取消保存后成功重连删除链路尚未执行，不能升级为“受限验证”。clippy 仍受一个无关既有告警阻断，深色视觉证据也尚未执行。
