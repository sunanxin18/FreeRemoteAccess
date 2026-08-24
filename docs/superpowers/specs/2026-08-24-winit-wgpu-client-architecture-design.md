# FreeRemoteAccess `winit + wgpu` 客户端架构设计

**Status:** Proposed; the user selected the `winit + wgpu` direction, pending review of this written specification  
**Date:** 2026-08-24  
**Scope:** Windows、macOS、Linux、Android、iOS；五个平台完成后再单独适配 HarmonyOS

## 1. 目标与边界

FreeRemoteAccess 是只连接操作系统原生远程服务的客户端：

- Windows 目标使用 RDP，默认 TCP 3389。
- Mac OS 目标优先使用 Apple 原生 Screen Sharing / ARD 高性能路径，标准 VNC
  仅作为最低优先级兼容路径，默认 TCP 5900。
- Linux 目标使用目标系统已经运行的标准 RFB/VNC 服务，默认 TCP 5900。
- 自动识别只执行有界的标准握手，不启动自定义代理、转发或服务端组件。

客户端不得要求、安装或运行远端守护进程、插件、驱动、代理或伴随程序。
Mac 登录只接受 Mac 本地用户名和密码；Apple ID、iCloud、IDS、APNs 和
QuickRelay 凭据始终不属于产品范围。

本次重构的目标是：

1. 正式产品只有一个 GUI 和一个可见窗口，连接页与远程桌面页是同一状态机。
2. 网络、认证、协议、解码、音频、输入映射和凭据生命周期全部保留在 Rust。
3. 远程画面不经过 Dart、序列化桥或整帧 UI 图片对象。
4. 使用持久化 GPU 纹理与脏矩形上传，窗口缩放和颜色转换由 shader 完成。
5. 删除 Flutter、Dart、Flutter FFI 插件和 `minifb` 正式渲染路径。
6. 当前无 Mac 连通性时只完成本地确定性验证；不得把离线测试描述为实时互操作成功。

## 2. 选型

正式依赖基线固定为：

- Rust `1.96.0`。
- `winit 0.30.13`：窗口、事件循环、键鼠、触控、DPI 和移动端生命周期入口。
- `wgpu 30.0.1`：Windows/DX12、macOS+iOS/Metal、Linux+Android/Vulkan。
- `egui 0.36.1`、`egui-winit 0.36.1`、`egui-wgpu 0.36.1`：连接表单和
  会话工具栏。禁用默认链接和通用剪贴板特性，只按产品需求逐项启用。
- `zeroize`：可清零的密码编辑缓冲和临时认证材料。

不使用 `winit 0.31` beta。依赖关闭不需要的默认特性，并按目标平台启用图形后端：

- Windows：`wgpu/dx12`。
- macOS、iOS：`wgpu/metal`。
- Linux：`wgpu/vulkan`，X11 与 Wayland 都由 `winit` 构建。
- Android：`wgpu/vulkan` 与 `winit/android-game-activity`。

`winit` 和 `wgpu` 是底层窗口/图形库，不承担远程协议决策。`egui` 只绘制低频
控件；远程桌面纹理由专用 `wgpu` pipeline 绘制，不能转换成 `egui::ColorImage`
或每帧重新注册 UI 纹理。

## 3. 应用形态

桌面平台构建一个 `freeremoteaccess` GUI 可执行文件。无参数启动进入连接页；现有
诊断 CLI 子命令继续由同一 Rust workspace 提供，但不再启动第二个 `minifb` 窗口。
`view` 与 `hpssview` 最终复用同一个 GUI/session runtime。

Android 和 iOS 使用最薄的平台工程负责安装包元数据、权限和启动入口；业务、页面、
事件循环、渲染和协议仍在 Rust。存在 Gradle/Xcode 文件不等于增加第二套 UI。

应用可见状态为：

```text
ConnectionForm -> Connecting -> SessionView -> Disconnecting -> ConnectionForm
                         |             |
                         +-> Failed <--+
```

- `ConnectionForm`：服务类型、主机、端口、用户名、密码以及仅 Windows 可见的域。
- `Connecting`：显示阶段和可取消操作，不显示协议秘密或远端返回的未净化文本。
- `SessionView`：远程纹理、断开、全屏、缩放、键盘、剪贴板、音频和网络状态。
- `Failed`：稳定错误码映射为简体中文；密码字段清空，主机和用户名可保留。

连接与远程桌面在同一个 `winit::Window` 中切换，不创建第二个产品窗口。

## 4. 两个正交适配维度

客户端运行平台和服务端协议是两个独立维度，任何组合都通过同一个 core 连接：

| 客户端平台适配器 | 可选择的服务端协议适配器 |
| --- | --- |
| Windows | RDP、Apple ARD、标准 RFB |
| macOS | RDP、Apple ARD、标准 RFB |
| Linux | RDP、Apple ARD、标准 RFB |
| Android | RDP、Apple ARD、标准 RFB |
| iOS | RDP、Apple ARD、标准 RFB |

“客户端是 Windows”不能自动推导服务端也是 Windows；服务类型只由连接配置和有界
协议探测决定。两组适配器通过平台无关类型交互：

```rust
pub trait ProtocolAdapter: Send + 'static {
    fn run(
        self: Box<Self>,
        context: ProtocolContext,
        commands: SessionCommandReceiver,
        events: SessionEventSink,
    ) -> Result<(), SessionError>;
}

pub trait PlatformServices: Send + Sync {
    fn create_audio_output(&self, config: AudioOutputConfig)
        -> Result<Box<dyn AudioOutput>, PlatformError>;
    fn request_software_keyboard(&self) -> Result<(), PlatformError>;
    fn set_fullscreen(&self, enabled: bool) -> Result<(), PlatformError>;
}
```

- `ProtocolAdapter` 的实现包括 `RdpAdapter`、`AppleArdAdapter`、`StandardRfbAdapter`。
- `PlatformServices` 的实现包括 Windows、macOS、Linux、Android、iOS。
- `SessionEngine` 独占一个 adapter，在协议线程中调用 `run`；UI 与 adapter 之间只有
  有界 command/event channel，adapter 不持有 `winit::Window`。
- 协议层不得 import 平台窗口、安装包、Gradle、Xcode、Win32 或 Android Activity 类型。
- 平台层不得构造 RDP/RFB/ARD wire message 或选择认证类型。
- UI 只向 `SessionEngine` 提交命令，不直接选择具体 adapter 实现。
- 音频设备、IME、全屏和系统权限经平台接口进入；网络协议与 framebuffer 保持共享。

## 5. Rust 模块边界

目标结构如下：

```text
src/
├── lib.rs
├── main.rs                       CLI/桌面 GUI 入口选择
├── core/
│   ├── connection.rs             连接验证、协议选择、SecretString
│   ├── session.rs                generation、状态、命令与事件
│   ├── frame.rs                  像素格式、矩形、RenderUpdate
│   └── error.rs                  稳定错误码和净化边界
├── protocols/
│   ├── mod.rs                    ProtocolAdapter trait 与 factory
│   ├── apple_ard.rs              Apple 高性能/ARD 适配
│   ├── rfb.rs                    标准 RFB 适配
│   └── rdp.rs                    IronRDP 适配
├── session/
│   ├── engine.rs                 协议线程生命周期与有界通道
│   └── backpressure.rs           更新合并、上限和 wake 策略
├── platform/
│   ├── mod.rs                    PlatformServices 与设备服务 traits
│   ├── windows.rs                Windows 权限、音频和打包入口
│   ├── macos.rs                  macOS 权限、音频和 bundle 入口
│   ├── linux.rs                  X11/Wayland 环境和音频入口
│   ├── android.rs                GameActivity、权限和软键盘
│   └── ios.rs                    UIKit 生命周期、权限和软键盘
├── ui/
│   ├── mod.rs                    GUI 公共入口
│   ├── application.rs            winit ApplicationHandler 与页面状态机
│   ├── connection_view.rs        egui 连接表单
│   ├── session_view.rs           会话工具栏和远程区域布局
│   ├── renderer.rs               surface、pipeline、帧调度、错误恢复
│   ├── remote_texture.rs         generation 绑定纹理与脏矩形上传
│   ├── input.rs                  坐标、键盘、触控、IME 到 SessionCommand
│   └── secret_buffer.rs          可清零密码编辑缓冲
└── vnc/                          迁移中的既有 ARD/RFB/MVS/媒体实现
```

每个模块只依赖相邻抽象：

- `core/` 不依赖 `ui/`、`platform/` 或具体协议实现。
- UI 只能消费 `SessionSnapshot`，发送 `SessionCommand`。
- UI 不得直接访问 socket、SRTP 密钥、密码或协议解析器。
- 协议适配器只能发布规范化 `SessionEvent` / `RenderUpdate`。
- renderer 不得解析协议，也不得根据服务类型改变 wire 行为。
- 平台 adapter 只能提供本地设备/生命周期能力，不能解释远端协议。
- `vnc/` 中已经建立的 P1/P2 generation 与完整帧门禁保持有效。

既有 `src/vnc/` 体量较大，本次先由 `protocols/apple_ard` 和 `protocols/rfb` 包装，
再按被修改模块逐步迁移；不得为了目录整齐一次性重写已验证的密码学和 wire parser。

## 6. 会话与渲染数据流

协议线程和 UI/渲染线程之间使用有界通道和 `EventLoopProxy` 唤醒：

```rust
pub enum RenderUpdate {
    Reset {
        generation: u64,
        width: u32,
        height: u32,
        format: RemotePixelFormat,
    },
    DirtyRect {
        generation: u64,
        rect: FrameRect,
        bytes_per_row: u32,
        pixels: Box<[u8]>,
    },
    Present { generation: u64 },
}
```

`RemotePixelFormat` 第一阶段只允许明确的 8-bit BGRA/RGBA sRGB 格式。所有像素数据
在构造 `RenderUpdate` 前验证：非零尺寸、矩形在当前表面内、行跨度不小于有效行、
乘法不溢出、总字节数与声明一致、总像素数不超过既有上限。

渲染规则：

1. `Reset` 只为比当前 generation 新的、已经由会话状态机确认的表面创建纹理。
2. `DirtyRect` generation 不匹配时静默归类为 stale，不写入 GPU。
3. `DirtyRect` 只上传声明区域；不能先扩展成窗口大小的 CPU 缓冲。
4. `Present` 只请求一次 redraw；空闲时事件循环等待，不进行固定 60 FPS 忙循环。
5. shader 完成 aspect-fit、实际尺寸/缩放、颜色通道转换和采样。
6. surface lost/outdated 时重建交换链，不能重建会话纹理或改变会话 generation。
7. surface out-of-memory 是终止性本地错误；远程连接必须有序断开。

CPU 解码的 MVS 完整帧至少需要一次 CPU 到 GPU 上传。标准 RFB 的 Raw/CopyRect 和
以后获得可信格式的 MVS partial 可使用脏矩形。没有直接证据时不宣称硬解零复制、
HDR、4K/60 FPS 或 Apple 视频流的传输方式。

## 7. 输入与动态分辨率

`RemoteViewportTransform` 是画面和输入共同使用的唯一坐标变换：

- 输入为当前窗口物理像素、DPI scale 和远端 surface 尺寸。
- 输出为钳制后的远端像素坐标以及是否落在 letterbox 区域。
- 指针、触控和动态分辨率目标都读取同一份 transform 快照。

P1 提交新分辨率时，远端尺寸、GPU 纹理、输入 transform 和 MVS decoder reset 仍然
作为一个 generation 转换。收到 exact acknowledgement 前继续显示旧纹理；超时或
拒绝时保留旧表面。P2 的 malformed/未知 partial 继续触发受限 full resync，不能进入
JPEG 路径或用猜测字段更新 GPU。

## 8. 凭据与安全

- 密码不进入 argv、环境变量、日志、recent connection、panic 文本或 UI 错误。
- 连接表单密码使用 `Zeroizing<String>` 等价包装；提交后移动进
  `secrecy::SecretString`，认证终态后清除 UI 缓冲。
- 禁止密码字段复制、拖放和持久化；粘贴需由用户显式动作触发。
- 协议提供的桌面名、剪贴板和错误文本在显示前做长度限制和控制字符过滤。
- 网络帧、纹理尺寸、GPU allocation 和音频队列都有硬上限与背压。
- 生产构建不启用 `wgpu` trace、GL 后备、WebGPU、链接打开和不需要的平台后端。
- `Cargo.lock` 必须提交；CI 运行 `cargo audit`、`cargo deny check`、格式和测试。
- GPU API 最终仍依赖系统驱动；“纯 Rust”不等于操作系统和显卡驱动属于内存安全 TCB。

第一阶段采用单进程以获得最低延迟。协议/解码运行在独立线程，但线程不是安全沙箱。
若后续威胁模型要求进程隔离，应另立设计，把协议 worker 放入平台沙箱并使用有界共享
内存；不得把未经设计的第二个 GUI 进程当作安全隔离。

## 9. 旧 UI 与 viewer 清理范围

实现阶段删除下列产品开发内容，不保留 Flutter 历史方案：

- `app/` 整个 Flutter/Dart 工程及五个平台 Flutter runner。
- `native/freeremote_ffi/` 与仅服务 Flutter 的 C ABI 头文件和测试。
- `toolchains/flutter.json`。
- Cargo workspace 的 `freeremote_ffi` member。
- CI 中 Flutter SDK、`flutter analyze/test/build` 和 Flutter artifact 路径。
- Flutter 专用 spec、plan、README、测试说明和生成注释。
- `minifb` 依赖、`viewer` feature 中的 `minifb` 绑定，以及两个 viewer 的
  `Window::new` / `update_with_buffer` 渲染循环。

生成目录和缓存只按精确路径清理，不删除协议 capture、逆向证据或用户本地凭据。
实现完成后的仓库不得包含 Dart 源码、`pubspec.yaml`、Flutter runner、Flutter plugin、
Flutter toolchain pin 或 Flutter CI action。新架构文档可以说明迁移事实，但不能保留一套
可被误认为仍有效的 Flutter 设计或构建方法。

## 10. 五平台构建与安装包

CI 使用原生 runner：

- Windows x64：release 可执行文件、便携 ZIP、MSI。
- macOS arm64+x64：`.app` 与 DMG；无签名产物明确标记为 unsigned。
- Linux x64：AppDir、`.deb` 与 AppImage。
- Android arm64：基于 GameActivity 的 release APK 与 AAB。
- iOS arm64：无签名 `.app` archive；可安装 IPA 仍由外部签名所有者处理。

平台工程只包含权限、图标、bundle metadata 和 Rust library/executable 启动胶水。
Windows/macOS/Linux 的产品入口相同；Android/iOS 的页面与 renderer 仍调用同一
`FreeRemoteApplication` 状态机。

HarmonyOS 不属于本轮实现。完成五平台后增加 `platform/ohos`，适配 OHOS 生命周期、
输入和 Vulkan surface；在实际构建和设备运行前不得声称 `winit` 原生支持鸿蒙。

## 11. 迁移顺序

迁移必须保持每个提交可测试：

1. 先增加纯 Rust UI 状态、凭据缓冲、viewport transform 和 `RenderUpdate` 单元测试。
2. 接入 `winit + egui + wgpu`，在本地显示连接页和确定性测试纹理。
3. 将 HPSS/RFB viewer 的画面、输入和动态分辨率接到新 session/renderer 契约。
4. 让无参数桌面入口启动单窗口 GUI，CLI viewer 复用同一 runtime。
5. 重写五平台 CI/打包，完成原生 host 的 compile/package gate。
6. 本地和 CI 全绿后删除 Flutter/FFI/minifb，更新 README 和架构文档。
7. 网络恢复后执行 Mac 实时认证、非黑帧、输入、动态分辨率和 Mac→PC 音频门禁。

不能先删除当前可运行入口再开始 renderer；新 GUI 至少能在本地显示连接页和测试纹理后
才能删除 Flutter。删除提交本身必须证明仓库中不存在 Flutter/Dart/minifb 产品引用。

## 12. 验收标准

### 本地离线门禁

- 连接页与会话页只存在于一个 `winit::Window`。
- 密码 redaction/清零、状态转换、stale generation、矩形边界和背压测试通过。
- headless/noop renderer 测试验证 reset、dirty upload、present 和 surface error 分类。
- Windows release 可启动，连接表单可操作，测试纹理覆盖完整远程区域且无黑边异常。
- `cargo fmt --all -- --check`、全 workspace 测试、无默认 feature 测试和 release build 通过。
- `rg --files` 门禁确认不存在 `.dart`、`pubspec.yaml`、Flutter runner、
  `freeremote_ffi` 或 Flutter toolchain 文件；源码、依赖和 CI 不再引用 `minifb`。
- 五平台 workflow 语法有效；不能把非本机交叉编译等同于完整安装包验证。

### 网络恢复后的 Mac 实时门禁

- 使用 Mac 用户名/密码完成 stock Screen Sharing 登录。
- 首个已提交 generation 显示非空、非全黑且尺寸正确的完整帧。
- 鼠标与键盘坐标对应当前远端 surface。
- 动态分辨率只在已验证门禁下启用，resize 后不显示旧 generation 帧。
- Mac→PC 音频仍能认证、解码和输出；P5 继续 fail-closed。
- 现场失败保留分类日志和可复现 capture，但不得记录凭据或会话密钥。
