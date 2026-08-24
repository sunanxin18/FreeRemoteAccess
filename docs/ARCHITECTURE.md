# FreeRemoteAccess 总体架构

本文描述 FreeRemoteAccess 的目标产品架构。详细接口、迁移顺序和验收门禁见
[`winit + wgpu` 客户端架构设计](superpowers/specs/2026-08-24-winit-wgpu-client-architecture-design.md)。

## 产品定位

FreeRemoteAccess 只实现远程登录客户端，直接连接未经修改的系统原生服务：

| 目标系统 | 客户端协议 | 服务端边界 |
| --- | --- | --- |
| Windows（Phase 1） | RDP | Windows 原生远程桌面服务 |
| Mac OS（Phase 1） | Apple Screen Sharing / ARD，VNC 最低优先级 | macOS 原生屏幕共享 |
| Linux（Phase 1） | RFB/VNC | Linux 已安装并运行的 VNC 服务 |
| Android（Phase 2） | 待原生远程入口验证 | 不预设私有服务端协议 |
| HarmonyOS（Phase 2） | 待原生远程入口验证 | 不预设私有服务端协议 |

项目不会在远端安装伴随程序、代理、驱动或守护进程。Mac 路径只使用本地账户的
用户名和密码，不接受 Apple ID/IDS 凭据。

## 两个正交维度

客户端运行平台与远端服务协议完全分离：

```text
PlatformServices                      ProtocolAdapter
Windows       ─┐                   ┌─ RDP -> Windows 原生服务
macOS         ─┤                   ├─ Apple ARD -> macOS Screen Sharing
Linux         ─┼─ SessionEngine ───┼─ RFB -> 标准 VNC 服务
Android       ─┤       / Core      ├─ Android target adapter (Phase 2)
iOS           ─┤                   └─ Harmony target adapter (Phase 2)
Harmony phone ─┤
Harmony PC    ─┘
```

因此 Windows 客户端可以连接 Mac 或 Linux，未来 HarmonyOS 客户端也可以连接
Windows；平台 adapter 只处理本地窗口、音频、输入法、权限和生命周期，protocol
adapter 只处理远端握手、认证、wire message 和会话事件。

## 分层

```text
系统原生 RDP / ARD / VNC 服务
                |
       ProtocolAdapter implementations
                |
      Core + SessionEngine / generation
          |                 |
   RenderUpdate         SessionCommand
          |                 |
   wgpu RemoteTexture   WindowHost / PlatformServices
          \                 /
          单窗口 Rust 应用
         egui 连接页/工具栏
```

- `src/core/`：平台和协议都无关的连接、会话、画面与错误类型。
- `src/protocols/`：RDP、Apple ARD、标准 RFB 服务端协议适配器。
- `src/session/`：协议线程、背压和事件分发。
- `src/platform/`：客户端平台的本地设备、窗口 host 与生命周期适配器。
- `src/ui/`：目标 `winit + egui + wgpu` 单窗口客户端。
- `src/vnc/`：迁移期间保留的已验证 Apple ARD、RFB、MVS、UDP/SRTP 实现。
- `src/framebuffer.rs`：受限 CPU framebuffer 与协议矩形应用。

正式仓库不保留第二套 GUI、Dart UI、C ABI UI bridge 或 `minifb` 渲染路径。

## 画面热路径

协议线程只提交 generation 绑定的表面重置和脏矩形。renderer 维护一张持久化 GPU
纹理，局部更新只上传对应矩形；窗口缩放、宽高比和颜色转换由 shader 执行。旧
generation 更新不会写入新纹理。没有新帧或 UI 动画时，事件循环保持等待。

## 安全边界

- 密码只在 Rust 可清零容器中短期存在，不进入命令行、日志或历史记录。
- 所有远端长度、尺寸、矩形、stride、allocation 和队列都有硬上限。
- UI 不接触 socket、协议密钥和未净化的远端文本。
- 协议适配器决定 wire 行为，renderer 不推断或修改协议。
- P5 PC→Mac 音频在用户名/密码产品路径下继续 fail-closed。

## 平台路线

第一阶段只完成 Windows、macOS、Linux 客户端和安装包，并优先验证连接 Windows、
Mac OS、Linux 原生服务。第二阶段再增加 Android、iOS、HarmonyOS 手机/平板/PC
客户端，以及 Android/HarmonyOS 被控目标的协议适配。

官方支持的平台使用 `WinitHost`；HarmonyOS 使用 ArkUI XComponent / NativeWindow
实现 `OhosHost`，与其他平台共享 Rust UI 状态、`wgpu` renderer 和 SessionEngine。
这表示架构可支持鸿蒙，不表示 upstream `winit` 已经提供 OHOS backend。

当前网络环境不能连接授权 Mac，因此本地编译、单测和 renderer 验证与 Mac 实时
互操作是两个独立门禁。离线通过不代表 ARD 实时画面已经验证。
