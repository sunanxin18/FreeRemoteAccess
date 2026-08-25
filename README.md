# FreeRemoteAccess

FreeRemoteAccess 是纯 Rust 远程登录客户端，直接连接操作系统原生远程服务，不要求远端安装伴随程序。

当前交付优先级：

| 客户端平台 | 状态 | 主要远端协议 |
| --- | --- | --- |
| Windows | 第一阶段 | RDP、Apple ARD、RFB/VNC |
| macOS | 第一阶段 | RDP、Apple ARD、RFB/VNC |
| Linux | 第一阶段 | RDP、Apple ARD、RFB/VNC |
| Android | 第二阶段 | 复用 Rust Core，补移动端窗口/生命周期适配 |
| HarmonyOS 手机/PC | 第二阶段 | 复用 Rust Core，补 ArkUI/NativeWindow 适配 |

Mac OS 连接优先使用 Apple Screen Sharing/ARD 原生路径，标准 VNC 仅作最低优先级兼容回退。Mac 认证只接受系统账户用户名和密码；不请求或保存 Apple ID、iCloud、IDS、APNs 或 QuickRelay 凭据。

## 桌面客户端

直接运行不带参数的程序会打开同一个 Rust 原生窗口：连接表单与远程画面都在该窗口内切换，不再包含 Flutter、Dart、C ABI UI bridge 或 `minifb` 第二套界面。

```powershell
cargo run --release
```

连接页提供服务端类型、主机、端口、用户名和密码。自动识别会根据目标类型选择协议；也可明确选择 Windows、Mac OS 或 Linux。密码保存在可清零内存中，不进入命令行、日志或配置文件。

命令行协议工具只通过 `--username USER` 接收非秘密的本地账户名；需要密码时会确认标准输入和提示终端均为真实终端，再进行无回显读取。重定向标准输入不会成为密码回退入口。自动化仅保留显式、有界的 `hpss-capture-v2 --credentials-stdin-v1` FRDSTD01 帧。

## 架构

```text
Windows / Mac OS / Linux 原生远程服务
                 |
       RDP / Apple ARD / RFB adapters
                 |
        Core + SessionEngine
          |                 |
     RenderUpdate      SessionCommand
          |                 |
   wgpu 持久纹理       winit 平台事件
          \                 /
          egui 单窗口客户端
```

- `src/core/`：与平台、协议无关的连接、会话、画面和错误契约。
- `src/protocols/`：RDP、Apple ARD、标准 RFB 适配器。
- `src/session/`：协议工作线程、事件分发和背压。
- `src/platform/`：本地窗口、输入、音频和生命周期边界。
- `src/ui/`：`winit + egui + wgpu` 单窗口 UI 与增量纹理上传。
- `src/vnc/`：已验证的 ARD/RFB、MVS、UDP/SRTP、AAC 实现。
- `packaging/`：Windows、macOS、Linux 原生安装包脚本。

完整边界和平台路线见 [总体架构](docs/ARCHITECTURE.md)。

## 构建和验证

Windows、macOS、Linux 均使用同一 Rust crate：

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --features gui
cargo build --locked --release
```

无 GUI 的协议工具构建：

```text
cargo build --locked --no-default-features --features cli
```

桌面安装包：

```text
# Windows PowerShell，需要 WiX v4
.\packaging\windows\build-msi.ps1

# Mac OS，生成 universal app 归档和 DMG
./packaging/macos/build-packages.sh

# Linux，保留 AppDir 并生成 AppDir 归档、deb 和 AppImage
./packaging/linux/build-packages.sh
```

GitHub Actions 工作流 `.github/workflows/build-desktop-installers.yml` 分别在原生 Windows、macOS、Linux runner 构建安装包，产物写入 `dist/<platform>/`。

### 发布与供应链门禁

当前所有桌面构建均标记为 **UNSIGNED / NOT FOR PUBLIC DISTRIBUTION**，只用于内部验证，不能作为公开发行版本。代码签名、公证、发布主体审核和 AAC 专利审核是外部发布前必须独立完成的 fail-closed 门禁。

每个平台的 `dist/<platform>/artifact-manifest.json` 是唯一 canonical artifact set。清单版本来自 `cargo metadata --locked` 的根包版本，并逐项记录产物、FDK AAC NOTICE、与 `Cargo.lock` checksum 完全一致的 `fdk-aac-sys` 原始 `.crate` 及 SHA-256。Windows 裸 GUI EXE 不能脱离同一 canonical artifact set 单独分发，必须与同目录 `THIRD_PARTY`、manifest 和校验和共同交付；portable ZIP、MSI、Mac OS app 归档/DMG、Linux AppDir 归档/deb/AppImage 则各自在包内携带相同的完整 NOTICE 与源码归档。

Fraunhofer FDK AAC 的软件版权许可不授予 AAC 专利许可。即使 NOTICE 和完整对应源码已随包交付，也不代表获得任何专利实施许可；在法律审核和适用地区的专利许可结论完成前，发布门禁保持关闭。

打包清单工具要求 Python 3.11 或更高版本（CI 固定 Python 3.13.14），供应链检查固定使用 `cargo-audit 0.22.2` 与 `cargo-deny 0.20.2`。Ubuntu 依赖来自官方 Snapshot Service 的 `20260810T000000Z` Jammy 快照，并按仓库 lock 文件指定精确版本；无 CA 的固定运行时镜像先使用同一快照中按 SHA-256 固定的 `ca-certificates` 包建立临时信任束，更新失败或来源/候选版本不符立即停止。构建脚本只接受 `Cargo.lock` 锁定依赖。AppImage 工具及 runtime 使用固定版本和 SHA-256，校验失败立即停止。

`RUSTSEC-2023-0071` 目前没有 patched version。FreeRemoteAccess 自有生产路径只使用 `rsa 0.9.10` 的 `RsaPublicKey` 执行服务端公钥加密，不持有 RSA 私钥，也不执行私钥解密；私钥解密仅存在于 `#[cfg(test)]` 的本地 mock server。IronRDP 的 CredSSP/NLA 依赖图还会编译 `rsa 0.10.0-rc.18` 以及 `sspi`/`picky`/`winscard` 中的私钥能力，但产品受控连接器只构造 `Credentials::UsernamePassword`，不向产品 API 暴露 Smart Card 或这些 transitive 私钥入口。CI 在运行审计前用两个 fail-closed guard 锁定自有源码的精确公钥 API，以及完整 RSA 版本、来源、feature、反向依赖、workspace/path package 和连接器凭据边界；任何依赖图漂移或新增 transitive API 入口都会阻断构建。审计只对该 advisory 作精确临时忽略。该例外最迟于 **2026-11-30** 复审；一旦上游提供 patched version，必须立即升级并移除 ignore，不能顺延或扩大范围。

最新三平台安装包、SHA-256、签名和启动证据见 [桌面客户端验证矩阵](docs/validation/winit-wgpu-desktop-matrix.md)。

## 当前协议状态

- Apple ARD：用户名/密码握手、HPSS、MVS、动态分辨率、UDP/SRTP 视频及 Mac→客户端音频接入统一会话层。实时兼容性仍需在目标 Mac 网络恢复后单独验证。
- 标准 RFB/VNC：Raw/CopyRect framebuffer、剪贴板、键鼠输入接入统一会话层。
- Windows RDP：基于纯 Rust IronRDP，启用 NLA/CredSSP，输出统一 BGRA 画面并支持物理扫描码、鼠标与桌面尺寸变更。
- PC→Mac 音频：用户名/密码的 Apple 原生服务没有已证实的接收路径，因此保持 fail-closed。

所有离线测试、构建与安装包成功都不能替代真实 Windows、Mac OS、Linux 服务端的互操作验证。
