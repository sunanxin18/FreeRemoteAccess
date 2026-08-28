# Windows 安全登录验证记录

**日期：** 2026-08-28

**范围：** Windows 产品组合、登录界面、本机安全存储；未读取
`CREDENTIALS.local.md`，未使用真实凭据。

## 构建产物

- 文件：`target/release/freeremotedesk-windows.exe`
- SHA-256：`31B0EBB9FE655928F4549BA28FBB156D211C4D57D02989C40EEE2AB2176C4203`
- 构建命令：`cargo +stable build --release -p freeremotedesk-windows`
- 结果：成功，release profile 完成，无编译警告。
- 说明：该 SHA-256 来自主代理在提交 `7409d65` 的全新 clean detached worktree 构建，不含共享工作树未提交改动。下述本机界面观察来自此前实际启动，未把该人工观察错误归因于这份未启动的 clean-build 哈希产物。

## 自动化验证

| 命令 | 结果 | 证据摘要 |
|---|---|---|
| `cargo +stable fmt --all -- --check` | 通过 | 全新 clean detached worktree、提交 `7409d65`，退出码 0。 |
| `cargo +stable test -p frd-platform-api` | 通过 | clean `7409d65`：2/2 个单元测试和 1/1 个 compile-fail 文档测试通过。 |
| `cargo +stable test -p frd-platform-windows -- --test-threads=1` | 通过 | clean `7409d65`：18/18 个测试通过；包含进程唯一 Windows Credential Manager 暂存、提交、读取、丢弃、删除及清理保护。 |
| `cargo +stable test -p frd-ui-model` | 通过 | clean `7409d65`：8/8 个测试通过。 |
| `cargo +stable test -p frd-app` | 通过 | clean `7409d65`：69/69 个测试通过；覆盖部分提交补偿、动作相关持久警告、暂存/提交/回滚、成功后取消保存和单次连接意图。 |
| `cargo +stable test -p frd-ui-egui` | 通过 | clean `7409d65`：12/12 个测试通过；覆盖窄屏布局、密码 Enter 单次提交、IME 与重复键过滤，以及身份页安全诊断映射。 |
| `cargo +stable test -p freeremotedesk-windows` | 通过 | clean `7409d65`：11/11 个二进制单元测试和 2/2 个依赖边界测试通过。 |
| `cargo +stable test --workspace -- --test-threads=1` | 通过 | 全新 clean detached worktree、提交 `7409d65`；工作区测试及文档测试退出码 0，需要未公开授权 fixture 的既有测试保持 ignored。 |
| `cargo +stable clippy --workspace --all-targets -- -D warnings` | **阻断** | clean `7409d65` 仍仅在既有已提交文件 `crates/frd-frame/src/surface.rs:35` 触发 `clippy::len_without_is_empty`；该文件不属于本功能，按任务边界未扩展修复。 |
| `cargo +stable build --release -p freeremotedesk-windows` | 通过 | 全新 clean detached worktree、提交 `7409d65`，退出码 0；产物哈希见上。 |

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

RED：新增回归最初因缺少 `RemoteSession.diagnostics`、`profile_persistence_warning` 和 `finish_session_cleanup_with_stores` 而编译失败。GREEN：共享工作树的 `cargo +stable test -p frd-app` 为 69/69 通过；`cargo +stable test -p frd-ui-model` 为 7/7，`cargo +stable test -p frd-ui-egui` 为 19/19，`cargo +stable test -p frd-shell-desktop` 为 47/47，且 `cargo +stable fmt -- --check` 通过。精确 Git 暂存区快照排除标题栏/图标脏改动后，`frd-app` 66/66、`frd-ui-model` 7/7、`frd-ui-egui` 12/12、`frd-shell-desktop` 35/35，以及 Windows 二进制 11/11 和依赖边界 2/2 均通过。随后提交 `7409d65` 机械修正 Windows `main.rs` 与 shell `lib.rs` 的 rustfmt 换行；主代理在该提交的 clean detached worktree 复核全 workspace fmt 与串行 workspace 测试均通过。

本轮没有改变 README 状态：授权 Mac GUI 的 TransportReady 提交和取消保存后重连删除仍未实测，平台矩阵继续保持 **开发中**。

## Scoped re-review：提交部分失败与动作相关警告

- Windows Credential Manager 的 `commit` 会先写正式 `profile/` 凭据，再删除 `pending/`；因此 pending 删除失败时虽然返回错误，正式凭据也可能已经改变。控制器现在在 `commit` 返回错误时同样执行提交前快照补偿：新建连接删除不确定的新凭据，覆盖连接先清除新值再恢复旧密码。
- 回归 store 精确模拟“正式写入成功、pending 删除失败”。新建测试证明没有孤立正式凭据或 pending；覆盖测试让原始提交和恢复提交连续两次在写入后返回错误，证明恢复后的旧密码不会被错误地再次删除。
- 会话状态不再使用单一布尔值，而是 `ProfilePersistenceWarning`：`SaveFailed`、`CredentialDeleteFailed`、`MetadataDeleteFailed`。对应中文分别说明保存失败、登录信息仍保留，以及密码已删除但最近连接记录清理失败；不会显示内部英文码。
- 强类型警告继续跨 surface generation 和首个完整帧进入远程会话保留，并在断开时清除。README 仍保持 **开发中**，Mac GUI 端到端未运行项不变。

RED/GREEN：强类型测试最初因 `ProfilePersistenceWarning` 不存在而编译失败；连续两次 post-write commit 失败测试随后以旧密码变为 `None` 正确失败。实现后聚焦 commit 补偿、取消保存三态和中文映射测试通过；共享工作树 `cargo +stable fmt -- --check`、`frd-ui-model` 8/8、`frd-app` 72/72、`frd-ui-egui` 19/19、`frd-shell-desktop` 47/47，以及 Windows 二进制 12/12、依赖边界 2/2、图标资源 2/2 均通过。排除无关脏改动的精确 Git 暂存区快照也通过 fmt，并通过 `frd-ui-model` 8/8、`frd-app` 69/69、`frd-ui-egui` 12/12，以及 Windows 二进制 11/11 和依赖边界 2/2。
