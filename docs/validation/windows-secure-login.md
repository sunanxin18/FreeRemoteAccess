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

## 最终跨层审计修复轮次

- 保存事务在提交前读取旧凭据。元数据写入失败时，新建连接删除已提交的新凭据，覆盖连接恢复旧凭据；若恢复本身失败，则删除新值并清理该会话的暂存项。该补偿仅覆盖进程内部分失败，不宣称具备崩溃恢复事务日志。
- 取消保存现在仅在凭据删除成功后删除非敏感元数据，避免凭据删除失败却让配置变成不可见孤儿。
- `profile_persistence_failed` 不再作为可直接显示的内部英文码；控制器保存独立的会话级失败标记，界面展示“登录信息未能安全保存；本次连接仍可继续，请稍后重试。”。该警告跨 surface generation 和首个完整帧进入远程会话保留，并在断开时清除。
- 正常清理完成通过产品 stores 重新读取并按最近成功顺序排列连接；错误页返回连接页也走同一路径。无 store 的 cleanup 入口仅保留为 `frd-app` 单元测试辅助，不是产品 API。

RED：新增回归最初因缺少 `RemoteSession.diagnostics`、`profile_persistence_warning` 和 `finish_session_cleanup_with_stores` 而编译失败。GREEN：共享工作树的 `cargo +stable test -p frd-app` 为 69/69 通过；`cargo +stable test -p frd-ui-model` 为 7/7，`cargo +stable test -p frd-ui-egui` 为 19/19，`cargo +stable test -p frd-shell-desktop` 为 47/47，且 `cargo +stable fmt -- --check` 通过。精确 Git 暂存区快照排除标题栏/图标脏改动后，`frd-app` 66/66、`frd-ui-model` 7/7、`frd-ui-egui` 12/12、`frd-shell-desktop` 35/35，以及 Windows 二进制 11/11 和依赖边界 2/2 均通过；该快照的全 workspace fmt 仅被本轮未暂存的两处既有格式差异阻断（Windows `main.rs` 导入与 shell `lib.rs` re-export）。

本轮没有改变 README 状态：授权 Mac GUI 的 TransportReady 提交和取消保存后重连删除仍未实测，平台矩阵继续保持 **开发中**。
