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

# macOS，生成 universal app、pkg、dmg、zip
./packaging/macos/build-packages.sh

# Linux，需要 cargo-deb、cargo-generate-rpm、AppImage 依赖
./packaging/linux/build-packages.sh
```

GitHub Actions 工作流 `.github/workflows/build-desktop-installers.yml` 分别在原生 Windows、macOS、Linux runner 构建安装包，产物写入 `dist/<platform>/`。

## 当前协议状态

- Apple ARD：用户名/密码握手、HPSS、MVS、动态分辨率、UDP/SRTP 视频及 Mac→客户端音频接入统一会话层。实时兼容性仍需在目标 Mac 网络恢复后单独验证。
- 标准 RFB/VNC：Raw/CopyRect framebuffer、剪贴板、键鼠输入接入统一会话层。
- Windows RDP：基于纯 Rust IronRDP，启用 NLA/CredSSP，输出统一 BGRA 画面并支持物理扫描码、鼠标与桌面尺寸变更。
- PC→Mac 音频：用户名/密码的 Apple 原生服务没有已证实的接收路径，因此保持 fail-closed。

所有离线测试、构建与安装包成功都不能替代真实 Windows、Mac OS、Linux 服务端的互操作验证。
