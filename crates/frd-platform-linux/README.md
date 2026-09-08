# Linux 平台服务

实现现有 `frd-platform-api`，不引用 Windows/macOS 平台实现。当前范围为 Linux
客户端基础服务；没有音频后端，不代表 Linux GUI、安装包或远程控制已经验收。

- `LinuxConnectionProfileStore::current_user_default/at_path` 保存非秘密连接元数据。
- `LinuxServerIdentityStore::current_user_default/at_path` 实现证书 TOFU，指纹变化拒绝覆盖。
- `LinuxCredentialStore::new` 仅在内存暂存未认证密码；准确匹配 session/profile 后
  才提交 Secret Service。调用者须使用现有凭据工作线程，禁止在绘制/输入线程调用。
- `LinuxSingleInstanceGuard::acquire_for_product/acquire_at` 持有文件描述符锁直到释放，
  不删除锁文件。非 Unix 平台的文件服务明确不可用。

XDG_DATA_HOME 必须为绝对路径；空值和相对值按 XDG 规则忽略，使用绝对 HOME 下的
`.local/share/FreeRemoteDesk`。路径遍历和链接拒绝；目录逐级通过 `openat` 打开，
应用私有目录/文件检查当前 UID 与私有权限。已有文件不能含硬链接；文件读取限制
4 MiB，密码限制 64 KiB。元数据写入有锁、同目录原子替换和文件/目录同步。

密码实现固定 `secret-service = 5.1.0`，使用 `rt-async-io-crypto-rust` 特性和 blocking
API 的 `EncryptionType::Dh`。仅调用标准当前客户端宿主 Secret Service；服务缺失或
锁定失败关闭，不调用外部命令，不生成密码文件，不自动解锁 collection。
索引为长度分隔字段的 SHA-256，不把账号地址直接写入 keyring label/attributes。

参考：[Secret Service API](https://specifications.freedesktop.org/secret-service/latest-single/)、
[固定 Rust crate API](https://docs.rs/secret-service/5.1.0/secret_service/blocking/)、
[XDG 目录规则](https://specifications.freedesktop.org/basedir/latest/)。

本机 ARM64 macOS 仅执行可移植/Unix 合成测试和注入 backend 事务测试，不能证明
Linux Secret Service 实际往返。Linux 专属 ignored 测试
`native_secret_service_authenticated_commit_roundtrip` 需要显式
`FRD_LINUX_KEYRING_TEST=1`，应在隔离 D-Bus/临时 keyring CI 中执行；使用唯一合成
条目并清理，禁止对用户现有 keyring 进行锁定/清空操作。
