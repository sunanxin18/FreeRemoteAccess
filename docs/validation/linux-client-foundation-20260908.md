# Linux 客户端基础服务验证（2026-09-08）

## 实现与支持范围

新增 `frd-platform-linux` 独立实现现有平台 API：XDG profile/pin、单实例锁、环境
provider 与 Secret Service 凭据事务。不引用 Windows/macOS 平台 crate，密码不写
普通元数据，缺少或锁定的 Secret Service 明确失败。

Linux 桌面 GPU 选择改为 Vulkan/GLES，相关 wgpu feature 仅在 Linux 目标开启；
Windows 仍选择 DX12，macOS ARM64 仍选择 Metal。此选择没有证明任何 Linux
窗口、GPU 设备或显示服务器已经运行成功。

## 已执行证据

- macOS ARM64 合成目录与注入 keyring：17 项平台服务测试通过。
- 同主机桌面 shell：214 项测试通过，Metal 选择保持。
- 固定 Rust 1.96.0 `cargo check --locked -p frd-platform-linux --all-targets`
  分别对 i686-unknown-linux-gnu、x86_64-unknown-linux-gnu、
  aarch64-unknown-linux-gnu 成功，包含真实 Secret Service 代码编译。
- x86_64-pc-windows-msvc 同命令成功，非 Unix 存储/非 Linux keyring 失败关闭。
- 根代理逐项审查了元数据 fd 相对访问、owner/mode/nlink、原子写与 flock，
  credential stage/commit/discard、DH/locked 分支及测试脚本；没有把审查称为运行证明。
- 格式、diff 检查与 shell 语法检查通过。

## 尚待执行

`tools/verify-linux-secret-service.sh` 在独立 dbus-run-session 与临时 XDG 目录中
运行唯一合成凭据的真实往返，前台 daemon 生命周期由脚本回收，不改 HOME，也不
接入用户已有 keyring。等待服务名最多 10 秒，测试过滤后必须恰有 1 项通过；零测试
不能算通过。脚本已接入 Ubuntu CI，但本次尚未取得运行结果。

新增 Linux 应用入口、原生窗口/输入、完整客户端 staging/verifier、三架构完整包、
真实 RDP 连接与 GUI 尚未完成。此前七目标 FFmpeg/Progressive 记录不能替代这些门禁。

最终完整工作区 `cargo test --locked --workspace`：exit 0，60 组，
1683 passed / 0 failed / 16 ignored。该数量来自 macOS ARM64 运行，不包含 ignored
Linux 原生 Secret Service 测试。

## 应用入口及完整包后续

Linux 应用入口 baebd80 的宿主测试通过 20 项单元及 2 项边界测试。完整工作区首次回归在旧的双平台组合根名单处失败；增加 Linux 的明确许可项，同时把 macOS/Linux 平台服务加入禁止依赖具体协议的检查后，Windows 两项架构回归通过。最终完整工作区 cargo test --locked --workspace 终态 exit 0，62 组、1705 passed / 0 failed / 16 ignored。宿主为 macOS ARM64，不能据此认定 Linux GUI 或 Secret Service 已运行。

完整 Linux stage/verifier 与三架构 CI 已接入实际应用构建、目标测试、ELF 校验、随包 decoder 加载和 tar 产物上传；9 项合成拒绝路径测试、shell 语法、YAML 三目标结构及格式检查通过。新流程尚未在 Linux 执行；完整包、窗口、GPU、输入及真实 RDP 门禁继续保持未验收。

## Linux 原生 Secret Service 与应用测试

2026-09-08 提交 `5cd73ea`，CI run `34198594600` 的 Ubuntu job `101972000994` 已终态 success。日志确认隔离 D-Bus / 临时 GNOME keyring 中 `secure_credentials::native_tests::native_secret_service_authenticated_commit_roundtrip` 实际执行并通过：1 passed / 0 failed / 0 ignored（不是默认忽略项）。该测试覆盖认证前仅暂存、认证提交后的新 store 读取及删除；使用固定合成密码，未访问用户真实凭据。运行日志保存在本机 `/tmp/frd-linux-native-services-5cd.log`。

同一 Ubuntu 原生 job 中 Linux 应用 20 项单元和 2 项依赖边界测试通过，完整工作区编译及安全核心测试通过。范围为 Ubuntu x86_64 的临时 keyring，不是全部 Linux 发行版、锁定 keyring 的交互解锁体验或实际远程登录证明。三架构完整客户端包和原生窗口仍独立待验收。
