# HarmonyOS 普通应用 RDP 组合验证（2026-10-15）

状态：源码及最终 FFI 已通过独立 Spec/Quality 审查。下文保留初始 `SOURCE_AND_FFI_READY` 的源码/宿主证据；后续真实 HAP 的 strip 绑定修正、重放、开发签名及手机/PC 普通表单运行见[安装包与运行验证](harmony-rdp-normal-hap-20261015.md)。**仍没有真实认证、远程首帧、持续刷新、输入或重连验收结果。**

## 普通应用路径与门禁

真实 C ABI `frd_ohos_app_create` → `AppEngine::for_platform` → 单一 `composition::spawn` → 原有 `AppEngine::spawn`。原有表单、沙盒 profile/pin/HUKS 服务、AppController、SessionHost、首帧实际呈现门、输入与生命周期继续由同一 owner 管理。

- 默认构建的协议工厂目录仍为空。新增 `normal-hap-rdp-validation` 默认关闭，仅普通应用在 `target_env=ohos` 且 `target_arch=aarch64` 时注册一个真实 `RdpProtocolFactory`。
- 使用既有 `RdpClientPlatformIdentity::Unix` 与 `RdpLegacyDecodePolicy::RequiredNative`。现有 factory 的 `LegacyOnly` 和空 EGFX provider 保持；不广告 H.264、HEVC、ZGFX/EGFX。连接器仍为 `compression_type=None`，此次未修改协议源码或 bulk 行为。
- 缺少原生核仍由既有 factory 在网络前拒绝，没有标量重试。继续使用返回 `BackendUnavailable` 的视频 loader 与 `NoAudio`；没有启用系统 AVC 或 FFmpeg 生产 loader。Legacy `SurfaceUpdate` 无需新增 video registry。
- 新模式与 `native-presentation-fixture`、`normal-hap-codec-fixture`、`normal-hap-codec-benchmark`、`normal-hap-avc444-profile`、`normal-hap-hevc444-profile` 在 debug 和 release 均编译互斥。
- 没有新增 ArkTS 页面、按钮、覆盖层、自动提交、自动连接或凭据注入；没有注册 Apple adapter，也没有修改桌面行为。

`tools/build-harmony.sh --normal-hap-rdp-validation` 转发精确 Cargo feature `frd-shell-ohos-ffi/normal-hap-rdp-validation`；未指定 `-p` 时选择该 shell，普通默认命令仍检查原四个中立 crate。`tools/build-harmony-hap.sh --normal-hap-rdp-validation` 沿用原 bundle 接收、暂存、Hvigor 与完整包 checker，仍要求独立接收的 FFmpeg bundle/manifest。该 builder 在新模式暂存前拒绝与仓库 `target` 不一致的 `CARGO_TARGET_DIR`，避免 CMake 默认导入旧库。

Hvigor 对 Rust FFI 启用 strip，FFmpeg 三库是原有例外。初始独立派生使用 `--strip-unneeded`，受控测试通过，但首轮真实 HAP 拒绝了该错误假设。核读实际插件后，Fix1 改为**所选官方 SDK** 的 `llvm-strip --keep-section=.permission SOURCE -oOUTPUT`，保持原库只读，并要求 HAP 唯一 FFI 条目与这个独立派生物字节完全一致，输出原始/派生 SHA-256。重复条目、损坏 ZIP、旧默认/夹具库、未 strip 字节或其它差异均拒绝。没有修改 CMake、native bridge、包 checker 或 strip 配置。Fix1 经过独立复审，随后真实 HAP 重放通过；首次失败及原受控测试历史均保留。

## 实际执行与结果

证据目录：`target/task4d1-rdp-normal-app-20261015/`。每个编号步骤保存 `*.started.json`、`*.result.json`、`*.stdout.log`、`*.stderr.log`，包含 argv、选定工具环境、PID、开始/结束时间、原始输出和真实退出码。`12-ohos-final-ffi.stderr.log` 的 shell trace 额外记录实际 SDK/linker/CC/CXX/AR/sysroot/CMake 环境与最终 Cargo 命令。

| 门 | 结果与原始证据 |
|---|---|
| 新组合选择语义 RED | `03-composition-semantic-red`：真实 root 依赖选择旧实现返回 `[]`，期待 `[rdp]`，1 failed / 3 passed；这才是语义 RED |
| 显式验证 shell 宿主完整测试 | `04-feature-host-green`：35 passed / 0 failed / 1 existing ignored，doc tests 0/0 |
| 默认 shell 宿主完整测试 | `05-default-host`：35 passed / 0 failed / 1 existing ignored，doc tests 0/0 |
| 原 native presentation fixture | `06-fixture-host`：34 passed / 0 failed / 1 existing ignored |
| 原普通 codec fixture | `11-old-codec-fixture`：42 passed / 0 failed / 1 existing ignored |
| 复用既有 RequiredNative/LegacyOnly factory 测试 | `08-factory-existing`：10 passed，覆盖原生核缺失在 config/session/runtime effects 前拒绝、默认不 probe、builder/identity 顺序及广告门 |
| 新模式互斥 | `10-conflict-{debug,release}-*` 十种组合全部以明确中文 `compile_error!` 拒绝；均为预期编译门，不计为行为 RED 或意外构建失败 |
| 构建脚本与包字节绑定 | `20-strip-binding-green`：4 tests passed，含多组子案例：精确 feature/default 传播、debug/release、未知/缺值拒绝、真实 HAP builder 流程、独立 strip 派生正例、旧库/原始库/重复/损坏拒绝，以及 canonical target 接受/不同目录在暂存前拒绝。耗时构建/包工具使用受控替身，内嵌 ZIP/字节检查由真实 Python 执行，不构成实际 HAP 构建证据 |
| 格式与 shell 语法 | `17-format`、`21-final-shell-syntax` 均 exit 0 |
| 官方 OHOS release 最终 FFI 链接 | `12-ohos-final-ffi` exit 0，真实 `Finished release`，耗时约 1 分钟；未重试或削减依赖 |

宿主 C ABI 测试真实调用 create/edit/snapshot/free/destroy，在默认与新 feature 宿主构建均确认表单保持 idle、没有 session、没有 synthetic 标记或自动连接。目标平台选择的 RDP-only descriptor 与不可用 video/audio 来自同一个私有组合根；OHOS ARM64 的 C ABI 测试尚未在设备执行。忽略项为既有独立进程 panic 测试子入口，没有隐去忽略或重标成功。

固定工具为 `/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin/{cargo,rustc,rustdoc}`；rustc `1.96.0 (ac68faa20 2026-05-25)`。`CARGO`、`RUSTC`、`RUSTDOC` 均显式绝对路径，`CARGO_NET_OFFLINE=true`，Cargo `--locked`；本任务新建 target 目录使用 `CARGO_TARGET_DIR=/Users/sun/FreeRemoteAccess/target/task4d1-rdp-normal-app-20261015/build`。官方 SDK 为 `/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony`。实际链接通过 `tools/build-harmony.sh --sdk <此SDK> --build --release -p frd-shell-ohos-ffi --normal-hap-rdp-validation`；该 scratch 库供独立审查，不能直接假定 HAP CMake 会导入它。

## 实际最终产物身份

- 原库：`target/task4d1-rdp-normal-app-20261015/build/aarch64-unknown-linux-ohos/release/libfrd_shell_ohos_ffi.so`
- 字节数：21,933,584；SHA-256：`f3e5eaf12d3c0d30577f56d929925fa726bca9250e34b4d91c86652450cf7279`
- 初始 `--strip-unneeded` 派生物：`libfrd_shell_ohos_ffi.expected-stripped.so`，15,613,776 bytes；SHA-256：`07c88e024b7fafdbfc0343e4c126b40d4e69be43d0226b5ef56f0fe238942c9a`。`22-ffi-strip-derivative` 静态命令 exit0，但它不匹配实际 Hvigor 行为，不能作为最终包身份；Fix1 正确派生物为15,613,552 bytes、SHA-256 `37846a22b556bc51dd48eb04f06817987c3cdebe1ec3073787912660f365e9d7`，实际包验证见后续记录。
- `15-ffi-elf` 实际 ELF：AArch64 / DYN，`DT_NEEDED` 精确为 `libnative_window.so`、`libhilog_ndk.z.so`、`libhuks_ndk.z.so`、`libohcrypto.so`、`libc.so`。导出包含 `frd_ohos_app_create/action/edit/password/snapshot` 等现有 ABI；本地符号存在真实 `composition::spawn`、`RdpProtocolFactory::descriptor/create`。符号存在与链接成功不等于目标运行。
- 最终 Cargo fingerprint 精确 feature 为 `[normal-hap-rdp-validation]`，依赖包含 `frd_protocol_rdp`，不含 FFmpeg fixture feature。见 `ffi-identity.json`，以及 `tool-executable-hashes.json`、从实际 dep-info 获取的 `actual-repository-link-input-hashes.json`。

## 保留的失败、裁决和边界

最早 `cargo generate-lockfile --offline` 虽然 exit 0，却升级了缓存依赖版本；这是不接受的锁文件候选，原始生成结果、完整差异和输出保存在 `rejected-generated.Cargo.lock`、`lock.delta.patch` 与 `01-lock.*`。随即恢复原锁文件；root 明确裁决只手工添加 shell package 下既有 `frd-protocol-rdp` 依赖项。最终 `accepted-lock.delta.patch` 仅这一行，没有版本、checksum、source 或其它解析变动，随后所有 Cargo 操作均 `--locked`。

`02-composition-red` 首次测试编译因作者误用 `SessionId::new` 失败；改为现有 `allocate` 后重新运行，只有 `03` 的断言失败才计为语义 RED。初版脚本测试还保留 macOS `/var` 与 `/private/var` 路径规范化修正前的失败。`13/14` 原始 FFI 直接字节比较的 RED/GREEN 属于后来被修正的包假设；root 核对 Hvigor strip 配置后，`19-strip-binding-red` 确认直接比较会误拒合法派生物，`20` 按官方 SDK 独立 strip 规则通过。它们都不是实际 HAP 验证结果。协议 crate 现有四项 dead-code warnings 原样保留。

源码/质量审查以及显式 HAP 的构建、签名、安装和普通表单子步骤已在后续独立记录完成。真实会话的生命周期/呈现/安全输入以及现有 TOFU/HUKS 路径仍需通过授权 Windows 登录、首帧、增量、输入、断开/重连验收。本文没有执行 profiler、benchmark、性能验收或生产资格提升。Apple 是另一任务；现有 native 核、wire、renderer/runtime、secure storage、ArkTS 和 native bridge 均不在本次修改范围。

## Windows 依赖边界回归修复（2026-10-15）

后续提交 `64d19d5` 的 Windows package run `38673701491` 在三个架构打包
任务的宿主 Rust 测试步骤失败。原始日志一致指向
`product_dependency_graph_preserves_protocol_and_legacy_boundaries`：递归扫描
实际找到上述鸿蒙平台组合根，但测试允许清单仍只有三个桌面入口。
这条断言没有证明 decoder 故障；原生 ARM64 runtime job 被跳过，不能计为通过。

修复仅在 `apps/freeremotedesk-windows/tests/dependency_boundary.rs` 的精确清单
加入 `crates/frd-shell-ohos-ffi/src/composition.rs`。递归收集、排序和完全相等
检查不变，没有目录/前缀豁免；协议中立依赖边界、Windows安全存储顺序及
工厂/EGFX门均保持。鸿蒙组合根的默认关闭、ARM64平台选择、Unix identity和
RequiredNative/LegacyOnly行为没有修改。

本机固定Rust1.96执行同一 `cargo test --locked --offline -p
freeremotedesk-windows --test dependency_boundary`：先实际复现1通过/1失败
（exit101），单行修复后2通过/0失败（exit0）；范围格式与diff检查通过。
原始输出、命令和退出记录保存在 `target/ci-ohos-boundary-20261015/`。
本机结果不替代修复提交的Windows CI、原生运行或鸿蒙真实会话验收。
