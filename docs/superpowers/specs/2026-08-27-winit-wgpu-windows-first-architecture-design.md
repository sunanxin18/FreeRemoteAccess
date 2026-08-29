# FreeRemoteDesk winit + wgpu Windows-first Architecture

## 状态

- 设计日期：2026-08-27
- 当前代码基线：`main@e2d1741`
- 当前实施平台：Windows 客户端
- 后续客户端平台：macOS、Linux、Android、HarmonyOS NEXT
- 远端服务端：系统原生 Windows RDP、macOS 屏幕共享/远程管理、Linux RFB/VNC
- GUI 决策：Windows、macOS、Linux 和 Android 使用 `winit + wgpu + egui`；HarmonyOS NEXT 使用 ArkTS/ArkUI + XComponent 宿主，并复用 Rust 会话与渲染核心

本规格取代把 minifb 查看器继续扩展为产品 GUI 的路线。现有 minifb 代码仅作为迁移期间的行为基线，达到 Windows 功能等价后必须从产品和测试路径中清除。

## 目标

1. 在 Windows 上实现一个单进程、单窗口的 FreeRemoteDesk 客户端。登录页、连接状态和远程桌面是同一个应用状态机的不同页面，不再启动第二套 GUI。
2. 用 wgpu 持久纹理和脏矩形上传替代 minifb 的全帧 CPU 缩放与呈现，保留当前 Apple HPSS/MVS 的画面、颜色、增量刷新、输入和动态分辨率行为。
3. 把“远端协议”和“本地客户端平台/UI”设计成两条正交扩展轴：新增协议不得修改平台宿主，新增客户端平台不得修改协议实现。
4. Windows 客户端最终通过三个相互隔离的适配器连接原生服务端：macOS 使用 Apple HPSS/MVS，Windows 使用 RDP，Linux 使用标准 RFB。
5. 为后续 macOS、Linux、Android 和 HarmonyOS NEXT 客户端保留经过约束的接入面，但当前不以未验证的平台代码拖慢 Windows 实现。
6. 保持客户端单边部署：禁止要求远端安装 FreeRemoteDesk 配套程序、代理、驱动、服务或插件。

## 非目标

- 不设计 FreeRemoteDesk 私有远程桌面协议。
- 不实现或部署任何服务端配套程序。
- 不把 Apple HPSS、RFB、RDP 拼接成一个混合会话，也不在连接过程中跨协议自动降级。
- 不把 Apple 普通 VNC 作为当前 Mac 连接回退；Mac 产品路径只使用当前已经验证的 Apple 用户名/密码 HPSS/MVS 会话。
- 不在本次 Windows-first 计划中交付 macOS、Linux、Android 或 HarmonyOS NEXT 安装包。
- 不重新推断或重写 Apple 线协议；Apple 适配器以当前 `main` 的真机验证实现和 ARD 3.10 证据为唯一迁移基线。
- 不重新开启 P5。用户名/密码 HPSS 下的 PC-to-Mac 音频输入继续保持 fail-closed，除非以后出现符合项目边界的新 Apple 原生协议证据。
- 不为低价值 GUI 细节堆积测试；只验证核心协议、帧契约、输入安全和平台生命周期。

## 两条正交扩展轴

系统不得形成“协议数 × 客户端平台数”的实现矩阵。协议和平台只能通过稳定的核心契约相交：

```text
客户端平台/UI 轴                                  远端协议轴

Windows/macOS/Linux ─ winit desktop shell         Apple ─ frd-protocol-apple
Android             ─ winit Android shell         Linux ─ frd-protocol-rfb
HarmonyOS NEXT      ─ ArkUI/XComponent host       Windows ─ frd-protocol-rdp
            │                                               │
            ├── AppIntent/InputEvent ─ frd-app/session ─────┤
            │                                               │
            └── SurfaceLease ─ compositor ─ renderer ◀── SurfaceUpdate
```

### 协议轴硬约束

1. 每种协议 adapter 在独立 crate 中实现，adapter crate 之间禁止依赖；只允许共同依赖无会话状态的 wire/media 叶子接口。
2. 一个会话只拥有一个 `ProtocolSession`。切换协议必须关闭并回收旧会话，再创建新会话。
3. 自动选择只在连接创建前解析为一个协议；不得同时启动多个协议、共享握手状态或在失败后隐式切换协议。
4. 协议适配器独占其 socket、认证、密码学、发送顺序、编解码器和能力协商。其他协议不得读取或复用这些内部状态。
5. `frd-session` 不解析 wire message，不根据 HPSS/RFB/RDP 分支处理帧或输入。
6. UI 和 renderer 不引用具体协议类型；它们只使用协议目录、能力、统一事件和统一错误。
7. 协议专属错误保留 `protocol_id` 和原始诊断，但不能改变其他适配器的状态。

### 平台/UI 轴硬约束

1. 平台宿主只负责窗口/Surface 生命周期、输入采集、DPI、IME、剪贴板、凭据输入、音频设备和安装集成。
2. 平台宿主不得包含 HPSS、MVS、RFB 或 RDP 解析与发送逻辑。
3. `frd-render-wgpu` 不依赖 winit、egui、ArkUI、窗口句柄或任一协议 crate，只接受 `SurfaceUpdate` 并录制 remote pass；`frd-compositor-wgpu` 只接受 owned Surface lease。
4. `frd-ui-model` 不依赖 GUI 工具包。egui 和 ArkUI 只把同一组状态与意图映射为各自控件。
5. Windows、macOS、Linux 共用 desktop winit shell；Android 使用单独的 winit 生命周期宿主；HarmonyOS NEXT 使用单独的 ArkUI/XComponent 宿主。
6. 新增客户端平台时，只能新增或扩展 shell、platform services、打包和平台验证，不得修改协议 crate。

### 可验证的隔离结果

- 新增协议时，允许修改该协议 crate、协议注册表和相应互操作验证；不应修改 renderer、UI 页面或平台宿主。
- 新增客户端平台时，允许修改该平台 shell、平台服务和打包；不应修改 Apple、RFB 或 RDP crate。
- 新增协议能力时，UI 通过 `SessionCapabilities` 显示或隐藏功能，不得编写 `if protocol == ...` 的业务分支。
- 所有具体协议工厂只在应用 composition root 注册；`frd-session` 在协议实现维度只依赖 `frd-protocol-api`，不依赖任一具体 adapter。
- 每个平台 app 只依赖自己的 shell/platform crate；Android、Wayland/AppKit/Win32、Harmony NDK 依赖必须放在目标专属 Cargo dependency 中，不能靠一个全平台 `--all-features` 二进制拼接。
- CI 用 `cargo metadata` 对照本规格的依赖允许表检查禁止边，防止后续提交把具体协议引入 UI/renderer 或把具体平台引入协议。

## 最终 Workspace 分层

```text
apps/
  freeremotedesk-windows/       Windows composition root 与发布入口
  freeremotedesk-macos/         后续 macOS composition root
  freeremotedesk-linux/         后续 Linux composition root
  freeremotedesk-android/       后续 Android 入口
  freeremotedesk-ohos/          后续 Rust cdylib 与 ArkUI 工程边界

crates/
  frd-core/                     领域类型、输入、连接状态、错误、凭据所有权
  frd-frame/                    CPU surface、像素格式、generation、damage
  frd-wire-rfb/                 无状态 RFB banner、字段和消息编解码叶子库
  frd-media-api/                协议无关音频帧和视频解码 port
  frd-protocol-api/             协议目录、工厂与会话接口
  frd-protocol-apple/           Apple 认证、HPSS、MVS、UDP/media codec 与动态分辨率
  frd-protocol-rfb/             标准 RFB/VNC
  frd-protocol-rdp/             IronRDP 集成
  frd-session/                  会话生命周期、命令/事件、背压、取消和线程回收
  frd-app/                      协议无关应用协调、单会话槽和能力合并
  frd-render-wgpu/              GPU context、远程纹理、上传和 remote pass
  frd-compositor-wgpu/          唯一 Surface、encoder、submit 和 present 所有者
  frd-ui-model/                 Connection/Connecting/Session/Failed 状态与意图
  frd-ui-egui/                  登录页、协议选择、状态栏和工具栏控件
  frd-shell-desktop/            Windows/macOS/Linux winit 事件适配
  frd-shell-android/            后续 Android winit 生命周期宿主
  frd-shell-ohos-ffi/           后续 ArkUI/XComponent C ABI 宿主
  frd-platform-api/             剪贴板、IME、凭据、音频等窄平台接口
  frd-platform-windows/         当前 Windows 平台服务
  frd-platform-macos/           后续 macOS 平台服务
  frd-platform-linux/           后续 Linux 平台服务
  frd-platform-android/         后续 Android 平台服务
  frd-platform-ohos/            后续 HarmonyOS NEXT 平台服务

tools/
  frd-protocol-lab/             保留 scan/info/shot/proxy/抓包等开发命令
  frd-legacy-minifb-lab/        迁移期独立行为对照，W6 删除
```

这是最终目录目标，不要求一次机械搬迁全部文件。迁移期间根 package 可以继续承载旧 CLI，但产品入口与新 crate 必须从第一阶段起遵守依赖方向。后续平台 crate 只在其实施阶段创建，禁止先填充无行为的占位模块。

### Crate 职责和允许依赖

| Crate | 唯一职责 | 允许依赖 | 禁止依赖 |
| --- | --- | --- | --- |
| `frd-core` | 协议无关领域类型、输入、状态、稳定错误 | 基础库 | socket、协议、winit、wgpu、egui |
| `frd-frame` | 像素、矩形、CPU surface、generation/revision | `frd-core` | 具体协议、窗口、UI |
| `frd-wire-rfb` | 无状态 RFB banner、基础字段和消息 codec | core 和字节 codec | socket 生命周期、认证选择、凭据、crypto、fallback、UI |
| `frd-media-api` | PCM/encoded video 等媒体数据和 decoder/output ports | core、frame | 具体协议、具体平台、窗口 |
| `frd-protocol-api` | 协议工厂和运行接口 | `frd-core`、`frd-frame`、`frd-media-api` | 具体协议、UI、平台 |
| 协议 crates | wire、认证、编码/解码、协议能力 | API、core、frame、无状态 wire/media API 和协议所需库 | 其他协议 adapter、winit、wgpu、egui、具体平台 |
| `frd-session` | 选择一个工厂、运行与回收会话、队列背压 | core、frame、media-api、protocol-api | 具体协议、窗口、GPU |
| `frd-app` | AppIntent、单会话槽、事件归并、媒体路由、有效能力 | core、media-api、session、protocol-api、ui-model、platform-api | 具体协议、窗口、GPU、egui |
| `frd-render-wgpu` | GPU context、远程纹理更新和 remote pass | core、frame、wgpu | 协议、egui、winit、平台窗口 |
| `frd-compositor-wgpu` | Surface lease、acquire、encoder、submit、present | core、renderer、wgpu、raw-window-handle | 协议、具体 UI 工具包 |
| `frd-ui-model` | 页面状态、表单状态与展示 DTO | core、protocol-api | 协议实现、窗口、GPU |
| `frd-ui-egui` | egui 控件绘制和意图生成 | ui-model、egui | 协议实现、socket |
| shell crates | EventLoop、窗口和宿主事件转换 | app、ui-egui、renderer、compositor、platform-api、winit 或 ArkUI bridge | 具体协议实现 |
| 平台 crates | OS 服务、媒体 backend 和发布集成 | platform-api、media-api、OS API | 协议实现 |
| app crates | 唯一 composition root | 所需具体实现 | 业务逻辑下沉回入口 |

## 协议注册与选择

协议不是动态 DLL 插件，而是由产品二进制编译并在 composition root 注册的工厂：

```rust
pub trait ProtocolFactory: Send + Sync {
    fn descriptor(&self) -> ProtocolDescriptor;
    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError>;
}

pub trait ProtocolSession: Send {
    fn run(self: Box<Self>) -> ProtocolExit;
}
```

`ProtocolFactory::create` 只校验配置并构造对象，不执行阻塞网络操作；连接和认证在 coordinator 启动的 adapter worker 中由 `ProtocolSession::run` 完成。`ProtocolRuntime` 仅提供本会话的命令接收端、事件发送端、帧 publisher、协议无关媒体 publisher 和取消信号。具体 socket、密码学对象和平台服务不会放入公共 runtime。应用层把 PCM mailbox 接到当前平台的 `AudioOutput`；Apple adapter 只拥有媒体 wire、SRTP、AAC-ELD codec 和时间戳，不直接打开 Windows/macOS/Linux 音频设备。

Apple 与标准 RFB 都建立在部分 RFB wire 结构上，但这不允许两个 adapter 互相依赖。二者只能共同依赖 `frd-wire-rfb` 这个无状态叶子 crate。该 crate 不拥有连接、凭据、安全类型策略、VNC fallback 或会话状态；Apple 30/33/35/36、Apple SessionCrypto、HPSS/MVS 只属于 Apple adapter，VNC Auth 和标准 framebuffer loop 只属于 RFB adapter。

连接页包含目标系统、地址、端口、协议和账号字段：

| 目标系统 | 默认协议 | 可选协议 | 当前产品规则 |
| --- | --- | --- | --- |
| Mac OS | Apple HPSS/MVS | Apple HPSS/MVS | 不提供 VNC 回退；用户名/密码；禁止 Apple ID |
| Windows | RDP | RDP | 连接系统原生 RDP 服务 |
| Linux | RFB | RFB | 连接系统已有 RFB/VNC 服务 |
| 自定义 | 无 | 用户显式选择已注册协议 | 不进行跨协议猜测 |

“自动选择”表示根据目标系统确定上表中的唯一协议，不表示同时探测或失败后切换协议。解析成功后，`ConnectRequest` 必须包含确定的 `protocol_id`；会话运行期间它不可变。

UI 从 `ProtocolCatalog` 获取名称、支持的目标系统、默认端口和静态提示，不直接导入三个协议 crate。握手完成后的实际能力由适配器发布：

```rust
pub struct SessionCapabilities {
    pub dynamic_resolution: bool,
    pub clipboard_read: bool,
    pub clipboard_write: bool,
    pub remote_audio: bool,
    pub text_input: bool,
}
```

能力默认关闭，只有当前适配器的成功协商可以打开。应用 coordinator 计算 `EffectiveCapabilities = ProtocolCapabilities ∩ PlatformCapabilities ∩ ProductPolicy` 后再交给 UI；例如协议支持音频但当前平台无输出设备时，UI 不能显示为可用。静态目录提示不得被当作运行时能力证据。

## 会话与数据流

```text
egui/ArkUI control
    │ AppIntent
    ▼
frd-app coordinator ─ SessionCommand ─▶ frd-session coordinator
    │ updates                                 │
    ▼                                         ▼ exactly one
frd-ui-model                          ProtocolSession
                                      │          │
                               SessionEvent   SurfaceUpdate
                                      │          │
                                      ▼          ▼
                                  frd-app    FrameMailbox
                                                 │ move ownership
                                                 ▼
                                         frd-render-wgpu
                                                 │
                                                 ▼
                                     PresentationCompositor
                                                 │ PresentationEvent
                                                 └──────────▶ host ─▶ frd-app
```

- `EventLoopProxy` 只发送 `WakeSession`、`WakeFrame` 等轻量唤醒事件；像素数据永远不进入 winit 用户事件队列。
- 每个连接创建独立 coordinator、命令队列、帧 mailbox、取消信号和 adapter，不使用跨会话全局可变状态。
- `frd-app` 是 SessionEvent、PresentationEvent 和平台能力的唯一归并点；shell 只转发宿主事件，UI model 只保存可展示状态。
- 连接、认证、socket read、MVS/RFB/RDP decode 和 shutdown/join 不得阻塞 winit 主线程。
- adapter 内部使用单一 writer 所有权串行发送输入、刷新、动态分辨率和协议控制消息。Apple CBC/HPSS 发送顺序不得由 UI 线程参与。
- 音频设备和媒体处理使用独立运行时；`cpal` 不再与 viewer feature 绑定。

### 公共会话命令

```rust
pub enum SessionCommand {
    Input(SessionInput),
    ViewportChanged {
        session_id: SessionId,
        generation: u64,
        viewport: PhysicalViewport,
    },
    ResolveServerIdentity {
        session_id: SessionId,
        challenge_id: u64,
        decision: ServerIdentityDecision,
    },
    ClipboardWrite(ClipboardPayload),
    Disconnect,
}

pub struct SessionInput {
    pub session_id: SessionId,
    pub generation: u64,
    pub event: InputEvent,
}
```

连接创建和协议工厂选择属于 coordinator API，不作为已运行 adapter 的普通命令。命令只发送给当前 adapter；关闭后未消费命令全部作废。

### 公共会话事件

```rust
pub enum SessionEvent {
    StageChanged(ConnectionStage),
    ServerIdentityChallenge(ServerIdentityChallenge),
    SurfaceGenerationChanged {
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
    },
    CapabilitiesChanged(SessionCapabilities),
    Clipboard(ClipboardPayload),
    AudioState(AudioState),
    Closed(SessionExit),
    Error(SessionError),
}

pub enum PresentationEvent {
    FramePresented {
        session_id: SessionId,
        generation: u64,
        revision: u64,
        completeness: FrameCompleteness,
    },
}
```

RDP 或 RFB/TLS 服务端证书通过系统信任链或当前 endpoint/protocol 的已保存 SHA-256 pin 时可以继续；未知自签名证书必须在同一窗口显示 `ServerIdentityChallenge`，由用户选择仅本次信任、信任并保存或拒绝。Windows 平台通过当前用户范围的 DPAPI 保护持久 pin，adapter 只接收连接时的已有 pin 快照和当前 challenge 的决策，不直接依赖平台存储。pin 不匹配必须 fail closed。为未知证书取证的预握手不得发送用户名、密码或完成 CredSSP/应用层认证；用户批准后必须重连，并由只接受该精确指纹的 verifier 完成正式握手。

协议握手成功和 UI 可展示远程桌面是两个不同事实。adapter 可以进入 `TransportReady`，但 compositor 在当前 generation 的完整 baseline 实际提交到 Surface 后才产生 `PresentationEvent::FramePresented { session_id, generation, revision, completeness }`。host 只转发该事件；`frd-app` reducer 只接受当前 session、当前 generation 且 `completeness == FullBaseline` 的回执，然后更新 UI model 进入远程页面。首帧门禁不重新定义 Apple Connected，也不让 session 依赖 renderer；它只防止旧会话回执或不完整增量把黑屏伪装成产品成功。

## 帧、generation 与背压契约

当前 `u32 0x00RRGGBB` 在小端内存中实际排列为 B、G、R、X。迁移后禁止用含糊的 `u32` 继续充当跨层像素协议：

```rust
pub enum PixelFormat {
    Bgrx8UnormSrgb,
    Bgra8UnormSrgb,
    Rgba8UnormSrgb,
}

pub enum SurfaceUpdate {
    Reset {
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
        format: PixelFormat,
    },
    Damage {
        session_id: SessionId,
        generation: u64,
        revision: u64,
        patches: Vec<PixelPatch>,
    },
    FrameBoundary {
        session_id: SessionId,
        generation: u64,
        revision: u64,
        completeness: FrameCompleteness,
    },
}

pub enum FrameCompleteness {
    Incremental,
    FullBaseline,
}
```

`PixelPatch` 的像素载荷使用不可 `Clone` 的所有权类型，首版可包装 `Box<[u8]>`，后续可在不改变公共语义的情况下替换为池化 lease。生产热路径必须把 payload move 入有界 mailbox，不得为了跨线程方便而复制整个 `SurfaceUpdate`。

规则如下：

1. `Reset` 将 renderer 绑定到新的 session/generation；revision 在 generation 内单调递增。会话关闭或切换时立即 detach/清除其远程纹理。
2. renderer 严格丢弃非当前 session 或旧 generation 的所有 Damage/FrameBoundary。
3. `Damage` 必须先通过矩形、stride、长度、溢出和 surface bounds 校验。
4. `FrameBoundary` 只能提交同 generation 中已经接收的 revision；它表示协议侧帧边界，不等同于 GPU 已 present。
5. `FullBaseline` 表示当前 generation 的整个 canonical surface 已初始化，能独立呈现；Apple adapter 只有在当前 generation 的完整 type-0 事务成功应用后才能发布它，type-1 单独到达或 no-op 永远不能建立初始 baseline。W2/W4 还保留当前 Apple 首屏 nonblack 诊断门；该诊断只属于 Apple adapter，不扩散为 RFB/RDP 的公共语义。
6. 已确认 geometry 必须通过 `ProtocolRuntime::begin_generation` 发布：先发送 `SessionEvent::SurfaceGenerationChanged`，再入队配对的 `SurfaceUpdate::Reset`，最后统一唤醒主循环；主循环先归并 session control，再消费 frame mailbox。`frd-app` 据此进入“正在切换分辨率”并暂停输入，renderer 只切换纹理和拒绝 stale update。新 FullBaseline 的 PresentationEvent 到达前，旧画面不得作为新 viewport、输入或首帧依据。
7. mailbox 同时限制消息数和像素字节数。增量更新不能任意丢弃：队列溢出时清除该 generation 的旧 damage，并由拥有 canonical CPU surface 的 producer 发布当前最新 full snapshot。
8. display geometry commit、decoder reset、surface generation 和输入映射必须作为同一个 generation 迁移完成。切换前通过同一 protocol writer 对旧 generation 执行一次 `ReleaseAll` 并清空本地按键/按钮状态；`frd-app` 给后续输入附加当前 session/generation，adapter writer 拒绝 stale input，禁止旧尺寸坐标写入新会话或新 generation。

当前 Apple MVS 内部的事务性 clone 不在第一批渲染迁移中同时重写。第一批性能目标是移除 render 锁竞争、每次 present 的全帧 CPU 缩放和无更新时的固定 60 Hz 全帧处理；MVS 解码内部拷贝在行为等价后单独优化。

### 稳态拷贝预算

- 协议解密和解码器内部必需的缓冲动作由各 adapter 自己负责和度量。
- 从 decoder/canonical surface 发布 damage 到 renderer 允许一次有意的脏矩形提取拷贝，payload 随后只移动所有权。
- renderer 执行一次 CPU-to-GPU `write_texture` 上传，不创建窗口尺寸的 CPU scaled buffer。
- 1:1 和缩放显示都由 GPU quad/sampler 完成；无新画面、无 UI 动画、无光标变化时不持续重绘。
- 后续只有实测表明 `write_texture` 成为瓶颈时，才引入 staging ring 或零拷贝 lease；不提前复杂化第一版。

## 固定实现基线

第一份实施计划固定以下兼容版本组，避免不同版本的 Surface 和 Android 生命周期 API 混用：

```text
Rust toolchain       1.96.0
winit                0.30.13
wgpu                 30.0.1
egui                  0.36.1
egui-winit           0.36.1
egui-wgpu            0.36.1
raw-window-handle     0.6.2
```

Windows 构建关闭不需要的 wgpu 默认后端，只启用 DX12 和 WGSL；其他后端在相应平台实施时通过目标专属依赖启用。升级 winit、wgpu、egui 兼容组或 raw-window-handle 前，必须先更新本规格中的生命周期/错误表并重跑 Windows Surface 门；升级到 winit 0.31 或新的 Harmony Vulkan 路径不能作为顺手依赖更新。

API 依据：[winit 0.30.13 ApplicationHandler](https://docs.rs/winit/0.30.13/winit/application/trait.ApplicationHandler.html)、[winit Android 0.30.13](https://docs.rs/winit/0.30.13/winit/platform/android/)、[winit pre_present_notify](https://docs.rs/winit/0.30.13/winit/window/struct.Window.html#method.pre_present_notify)、[wgpu 30 CurrentSurfaceTexture](https://docs.rs/wgpu/30.0.1/wgpu/enum.CurrentSurfaceTexture.html)。`egui-wgpu 0.36.1` 与 wgpu 30.0、winit 0.30.13 属于同一兼容组；当前本地 Rust 1.96.0 满足其 Rust 1.95 最低版本。

## wgpu 渲染与合成分层

`frd-render-wgpu` 不拥有窗口 Surface，只包含：

1. `GpuContext`：`Instance`、`Adapter`、`Device`、`Queue`，可在窗口 Surface 暂时销毁时继续存在。
2. `FrameUploader`：校验并上传当前 generation 的 damage。
3. `RemoteTexture`：远端 generation、像素格式、纹理和采样器。
4. `RemotePass`：向调用者提供的 `TextureView`/`CommandEncoder` 录制 aspect-fit、黑边、缩放和色彩/alpha pass。

`frd-compositor-wgpu` 是唯一的 swapchain 所有者，包含 `PresentationSurfaceLease`、`wgpu::Surface`、配置和物理尺寸。它独占 `get_current_texture`、唯一 `CommandEncoder`、queue submit 和最终 present，并按以下顺序合成：

```text
acquire surface texture
  → RemotePass
  → optional overlay recorder（desktop/Android 为 egui，Harmony 为空）
  → PresentationHooks::before_submit
  → submit
  → present
  → PresentationEvent
```

desktop shell 持有 `egui-winit` 输入状态和 `egui-wgpu::Renderer`，把 egui overlay 作为 compositor 的受控 recorder 传入；它不取得 Surface 所有权。shell 同时注入平台中立 `PresentationHooks`：Wayland 实现在绘制录制完成、submit/present 之前调用 `Window::pre_present_notify()`，其他平台为空操作。HarmonyOS NEXT 的 ArkUI 不进入 wgpu render graph，compositor 只录制 RemotePass。只有 compositor 能在成功 present 后发布 `PresentationEvent::FramePresented`。

### Surface lease 安全契约

公共接口禁止传入没有所有权保证的裸 raw handle。`PresentationSurfaceLease` 必须同时持有创建 Surface 所需的 handle provider 和强生命周期令牌，确保原生窗口的有效期覆盖 `wgpu::Surface`：

- desktop：lease 持有 `Arc<winit::Window>` 或等价 owned Surface target；
- Android：lease 只在一次 `Resumed` 到对应 `Suspended` 之间有效，`Suspended` 回调内先停止 present 并同步 drop Surface；
- HarmonyOS NEXT：attach 时增加 `OHNativeWindow` 引用，detach 时先停止 present、drop Surface、清除句柄访问，再 unreference；
- `PresentationSurface` 通过显式 `detach`/`Drop` 保证先销毁 Surface、后释放 lease，不依赖容易被字段重排破坏的隐含析构顺序。

renderer 和 compositor 都不保存具体协议类型。renderer 也不保存 `winit::Window`；平台只通过 owned lease 创建/重建 Surface，因此 HarmonyOS NEXT 不需要伪造或 fork winit。

Windows 第一版默认使用 DX12。远端 CPU `Bgrx8UnormSrgb` 上传到 `Bgra8UnormSrgb` 纹理时，shader 强制 alpha 为 1，避免当前 X=0 造成透明/黑屏；sRGB 转换只能发生一次。GPU 负责 aspect-fit 和采样，输入与 shader 共用同一个物理像素 viewport 计算结果。

### wgpu 30 Surface/Device 恢复

- 零尺寸、`Occluded`：暂停 present，保留远端纹理。
- `Timeout`：跳过本次 frame，等待下一次唤醒。
- `Outdated`：在所有旧 `SurfaceTexture` 释放后重新 configure。
- `Lost`：使用仍有效的 lease 重新创建 `wgpu::Surface`，然后 configure；仅 configure 不足以恢复。
- `Suboptimal(texture)`：当前 texture 仍可提交，present 后再按最新物理尺寸 configure。
- `Validation`：读取 error scope/uncaptured-error 诊断，放弃本帧并进入结构化错误路径。
- device lost：重建 Device、Queue、所有 GPU texture/pipeline，再从 canonical CPU surface 发布 full snapshot。
- 内存耗尽属于 Device/uncaptured-error 路径，不作为 Surface acquisition 枚举分支；无法重建时安全关闭当前会话并给出明确诊断。

远端画面禁止转换为 `egui::ColorImage`，也禁止经 UI 状态逐帧复制。

## 单窗口 UI 与平台衔接

产品只有一个登录与会话入口：winit 创建的主窗口。命令行不再拥有独立的连接流程，也不直接启动协议 viewer；它只生成一次性的 `LaunchOptions`，由应用层合并为登录页的 `ConnectionDraft`：

```rust
pub struct LaunchOptions {
    pub target_system: Option<TargetSystem>,
    pub address: Option<String>,
    pub port: Option<u16>,
    pub protocol: Option<ProtocolId>,
    pub username_provider: Option<CredentialProviderId>,
    pub password_provider: Option<CredentialProviderId>,
    pub connect_when_complete: bool,
}

pub struct ConnectionDraft {
    pub target_system: Option<TargetSystem>,
    pub address: String,
    pub port: Option<u16>,
    pub protocol: ProtocolChoice,
    pub username: String,
    // SecretBuffer 与可复制草稿分开持有，不属于这个结构。
}
```

- 无参数启动、CLI 缺少任一必填字段或凭据 provider 未能提供值时，都显示同一个 `ConnectionForm` 让用户补齐，不因缺少登录信息退出程序。
- CLI 只允许地址、端口、目标系统、协议、凭据 provider 名称和 `--connect` 等非秘密参数。实际密码禁止放入 argv；它只能来自 GUI 密码框、环境凭据 provider 或现有受保护的 stdin 凭据帧。
- `connect_when_complete` 只有在目标系统、地址、端口、协议选择和该协议声明的凭据字段全部通过校验时才触发一次自动连接。校验不完整或 provider 失败时留在登录页，精确标记字段错误，不启动 worker。
- CLI 与 GUI 都只产生同一个 `ConnectionSubmission`，再由 `AppIntent::Connect` 进入 `frd-app`；不存在第二套 CLI session coordinator、第二个 GUI 或协议专属登录窗口。
- 断开或失败后回到同一登录页，保留非秘密字段和用户名，清除密码。重新连接仍复用当前窗口和单会话槽。

`frd-ui-model` 保存以下页面状态：

```text
ConnectionForm
  → Connecting(stage, diagnostics)
  → AwaitingFirstFrame(stage, diagnostics)
  → RemoteSession(capabilities, toolbar state)
  → Failed(error, retained non-secret fields)
```

- 所有页面在同一个 winit EventLoop 和同一个窗口内切换。
- `ConnectionForm` 以 `ConnectionDraft` 为唯一可复制表单状态；密码由同层的不可 `Clone` `SecretBuffer` 单独持有，不进入草稿、日志或可复制快照。连接时把秘密所有权移交 session，握手结束或失败后立即清零。
- 未知 TLS 服务端身份使用当前窗口中的确认页/覆盖层，不另开原生对话框或第二个 GUI；只显示 endpoint、subject/issuer、验证失败原因和 SHA-256 指纹，绝不显示或记录凭据。
- 失败返回连接页时只保留地址、端口、目标系统、协议和用户名，不保留密码。
- egui 只处理低频登录表单、设置、状态和工具栏。远程桌面是 wgpu 专用 pass。
- egui 先消费控件事件；仅当事件位于远程 viewport 且未被 UI 消费时，才转换为远端 `InputEvent`。
- UI 通过 `AppIntent` 驱动 session，不直接持有 socket、crypto、decoder 或 adapter。

### 客户端平台实现

| 客户端平台 | 窗口/UI 宿主 | wgpu 首选后端 | Surface 生命周期 |
| --- | --- | --- | --- |
| Windows | winit + egui | DX12 | Win32 窗口生命周期 |
| macOS | winit + egui | Metal | 主线程 AppKit 生命周期 |
| Linux | winit + egui | Vulkan | X11/Wayland，Wayland present 通知封装在 shell |
| Android | winit GameActivity + egui | Vulkan，GLES 兼容路径 | `resumed` 创建、`suspended` 销毁 Surface |
| HarmonyOS NEXT | ArkTS/ArkUI + XComponent | 首个 POC 使用稳定 wgpu GLES | XComponent 回调管理 `OHNativeWindow*` |

Windows、macOS、Linux 和 Android 可以复用 `frd-ui-egui` 的行为与控件，但窗口生命周期分别由 shell 适配。HarmonyOS NEXT 使用 ArkUI 控件呈现相同 `frd-ui-model` 状态，并通过稳定、粗粒度 C ABI 发送意图；ArkTS 不参与帧传输。

### 平台字体与缺字回退

普通 UI 文本由 shell 按平台和界面语言建立字体链，禁止用一套内嵌字体覆盖平台习惯：

| 平台 | 拉丁/UI 主字体 | 简中/繁中 | 日文 | 韩文 |
| --- | --- | --- | --- | --- |
| Windows | Segoe UI | Microsoft YaHei UI / Microsoft JhengHei UI | Yu Gothic UI | Malgun Gothic |
| macOS | San Francisco | PingFang SC/TC | Hiragino Sans | Apple SD Gothic Neo |
| Linux | 桌面可用的默认 Sans | 系统 Noto Sans CJK 对应区域字形 | 同左 | 同左 |
| Android | Roboto | 系统 Noto Sans CJK SC/TC | 系统 Noto Sans CJK JP | 系统 Noto Sans CJK KR |
| HarmonyOS NEXT | ArkUI 平台默认字体；Rust shell 不替换 | 由 ArkUI 字体级联处理 | 同左 | 同左 |

`frd-shell-desktop` 只读取当前界面语言对应的一套 CJK 系统字体，避免同时把多份大型 TTC 读入内存。当前产品文案是简体中文，因此使用 `zh-Hans` 字形顺序；未来本地化必须先切换界面语言，再切换统一汉字的区域字形，不能仅凭 Unicode 码点猜测中文或日文。若平台主字体不可用，内嵌的 Noto Sans SC 可成为比例字体主字体；存在平台字体时，它只处于普通文本和等宽文本链的最后一级。Material Symbols 使用独立命名字体族，不参与普通文字回退。

内嵌 Noto Sans SC 字体、SHA-256 来源记录和 SIL OFL 1.1 必须一同分发。若后续恢复 iOS 客户端，其原生 UI 宿主直接使用系统 San Francisco 和系统区域 CJK 级联，不从 Rust 包复制 Apple 系统字体。

Android shell 必须把重复的 `resumed`/`suspended` 当作幂等事件：仅在没有有效 lease 时 attach，仅在存在 lease 时 detach；绝不能复用上一轮 Activity 的 Surface。

HarmonyOS NEXT C ABI 只暴露 opaque engine handle、版本化 `#[repr(C)]` POD 命令/状态和明确所有权的 buffer，不直接暴露 Rust enum、`String`、`Vec`、trait object 或 wgpu 类型。销毁顺序固定为：停止 present，销毁 `wgpu::Surface`，清除 Rust 中的窗口访问，再释放 `OHNativeWindow` 引用。正式支持前必须完成 HAP、XComponent、前后台、输入、IME、窗口缩放和真机 present POC；未通过 POC 前不得宣称已支持。

## 输入边界

平台 shell 将 winit/ArkUI 事件规范化为协议无关输入：

```rust
pub enum InputEvent {
    PointerMove { remote: PixelPoint },
    PointerButton { button: PointerButton, state: ButtonState },
    Wheel { delta: WheelDelta },
    PhysicalKey { code: PhysicalKeyCode, state: KeyState, modifiers: Modifiers },
    Text { utf8: String },
    ReleaseAll,
}
```

adapter 再把统一输入编码为 Apple/RFB keysym、RDP scancode 或协议支持的 Unicode 事件。平台 keycode、winit enum 和 ArkUI enum 不得进入协议 API。

必须保留当前已经真机验证的 `PointerInputState` 安全语义：

- 指针在远程 viewport 外或窗口不活跃时不发送远端移动/按下事件。
- 按住鼠标离开 viewport 时最多发送一次 release。
- 本地按钮仍按住时重新进入 viewport，不重新伪造按下。
- 窗口失焦、会话切换或断开时统一 `ReleaseAll`，防止远端卡键或卡按钮。
- 坐标映射使用 drawable 物理像素，不使用 egui logical points；同一 `ContentViewport` 同时用于渲染和输入。

动态分辨率的 viewport debounce、Apple `0x09`/确认和 generation commit 继续属于 Apple adapter。公共 session 只发送 `ViewportChanged` 并接收新的 `SurfaceUpdate::Reset`，不得理解 Apple wire 语义。

## 生命周期与错误处理

主线程拥有 winit EventLoop、窗口、egui、wgpu compositor、输入和 present。后台职责为：

- session coordinator：创建 adapter、状态转换、取消和资源回收；
- protocol reader/decoder：网络接收、解密、解码和 surface 发布；
- protocol writer：独占发送顺序；
- audio runtime：设备回调与媒体队列。

关闭流程必须幂等：停止接收新输入，触发取消，唤醒阻塞 I/O，关闭 writer，等待 reader/decoder 和音频退出，丢弃 mailbox，最后发布一个 `Closed`。新连接只能在旧会话资源回收完成后启动，以防两个 client/session 同时操作远端。

统一错误包含 category、stage、稳定 code、可重试性、可选 `protocol_id` 和敏感信息清理后的诊断。UI 根据结构化字段显示状态，不解析字符串。协议失败只关闭当前 adapter；不触发跨协议回退。当前规格不新增隐藏重试策略。

## 安全边界

- 只连接用户指定的系统原生服务；不安装或运行服务端辅助程序。
- Mac 仅使用本地 macOS 用户名/密码，禁止请求、持有或使用 Apple ID、iCloud、IDS、APNs 或 QuickRelay 身份凭据。
- 凭据只从非回显 UI/本地 credential provider 进入，绝不进入 argv、日志、capture、panic 文本、设计文档或测试 fixture。
- 密码缓冲不可普通 Clone；移交 adapter 后在认证完成、取消或错误路径清零。
- 每个 adapter 独占密码学状态和发送器；禁止跨协议共享 session key、nonce、sequence 或 replay state。
- 协议目录是编译时注册表，不加载未签名的第三方协议 DLL。

## Windows-first 迁移阶段

### W1：建立正交契约

- 将根项目转换为可增量迁移的 Cargo workspace。
- 建立 `frd-core`、`frd-frame`、`frd-media-api`、`frd-protocol-api`、`frd-session`、`frd-app` 的最小真实接口。
- 从当前代码提取物理 viewport、协议无关输入、generation/revision/damage 和有界 mailbox。
- 旧 minifb 路径移入独立 `tools/frd-legacy-minifb-lab` package，minifb 只作为该 package 的可选依赖；它只做串行行为对照，不接收新功能。Windows 产品 composition root 从 W1 起永不依赖、注册或启动它。
- Windows 应用以 `ActiveSessionSlot` 拒绝同进程第二个连接，并以用户级 single-instance guard 拒绝第二个产品进程；旧会话未完成回收时 Connect 保持不可用。

### W2：隔离 Apple adapter

- 把当前 `main@e2d1741` 的 Apple 认证、HPSS/MVS、UDP/media wire、SRTP、AAC-ELD、动态分辨率和 writer 顺序迁入 `frd-protocol-apple`；Windows 音频设备输出留在平台/audio runtime。
- 不更改 Apple wire framing、认证算法、MVS type-0/type-1 解码、色度平面顺序、P3/P4 或 P5 fail-closed 结论。
- `src/vnc/client.rs` 不得整文件搬迁：banner、ServerInit 和基础 message codec 进入 `frd-wire-rfb`；Apple 30/33/35/36、SessionCrypto 和 HPSS 进入 Apple adapter；VNC Auth 和标准 framebuffer loop 留给 RFB adapter。
- Apple factory 遇到缺少 Apple 用户名/密码安全类型时 fail closed，绝不调用标准 VNC fallback。这是已确认的产品选择策略收紧，不改变 Apple 认证算法或已验证 wire 字节。
- 将现有 CPU surface 输出适配为 `SurfaceUpdate`；先保留已验证 decoder 内部行为。
- 当前标准 RFB 路径不得被混入 Apple adapter。

### W3：Windows winit/wgpu 单窗口

- 新建 Windows composition root、`frd-render-wgpu`、`frd-compositor-wgpu`、`frd-ui-model`、`frd-ui-egui`、`frd-shell-desktop` 和 Windows platform services。
- 先以确定性测试纹理验证 resize、DPI、Surface 恢复、色彩和单窗口页面切换。
- 接入 Apple mailbox；实现首真帧门禁、GPU 脏矩形上传和远程画面 pass。
- 接入输入、IME 能力声明、剪贴板能力声明和动态分辨率 viewport。

### W4：Apple 真机等价门

- 对授权 Mac 验证登录、首帧、全帧、type-1 增量、颜色、远端刷新、鼠标、键盘、离窗静默、失焦释放、resize/dynamic generation 和断开回收。
- 验证 single-instance guard 和 `ActiveSessionSlot` 确保只有一个产品窗口和一个会话；legacy/new A/B 真机验证必须串行，启动下一实例前先完成上一实例断开与进程退出。
- 未通过等价门前，不删除 legacy viewer，也不同时优化 MVS 内部事务拷贝。

### W5：独立接入 RFB 和 RDP

- 标准 RFB 只实现为 `frd-protocol-rfb`，不得调用 Apple adapter。
- RDP 只通过 `frd-protocol-rdp` 封装 IronRDP。Windows-first 互操作门先使用可验证的 CPU decoded `SurfaceUpdate`；若 H.264/GFX 性能需要平台硬解，encoded sample 只能通过 `frd-media-api` 交给 Media Foundation 等平台 backend，禁止在 RDP adapter 中加入 Windows/macOS/Android/Harmony 分支。GPU external-image import 需单独 POC 后扩展 frame contract，不在本次迁移中推断。
- 两个 adapter 都只发布相同的 `SurfaceUpdate`/`SessionEvent`，只接收相同的 `InputEvent`。
- 各自完成与系统原生 Linux VNC 和 Windows RDP 服务端的独立互操作门。

### W6：Windows 产品收口

- 所有产品连接路径切换到新 app/session/shell/compositor/renderer。
- 删除 `frd-legacy-minifb-lab`、minifb 依赖、`viewer` 混合 feature、minifb key mapping、旧 viewer 渲染循环和相关过期测试/文档。
- 开发用 CLI/抓包工具保留为 headless `frd-protocol-lab`，不得依赖产品 GUI。
- 分离音频 feature，并构建 Windows release、ZIP 和 MSI 安装包。

后续 macOS/Linux 复用 desktop shell；Android 和 HarmonyOS NEXT 分别先完成平台 Surface/输入/打包 POC，再进入正式适配。后续平台实施不得反向修改已通过互操作门的协议实现。

## 验证策略

遵守“仅核心协议和契约测试”的项目规则，不建设大规模 GUI snapshot 或为每个平台复制相同业务测试。

### 最小自动验证

1. generation 切换与 stale update 丢弃。
2. damage bounds、stride、长度和 BGRX/BGRA alpha/色彩契约。
3. mailbox overflow 转换为当前 generation 的 full snapshot，不能留下破损增量链。
4. viewport 物理像素映射，以及离窗、失焦、断开时的 release 语义。
5. 动态分辨率 generation 原子提交继续使用现有 Apple 核心测试。
6. 协议目录只解析出一个 adapter，非法目标系统/协议组合在连接前失败。
7. FullBaseline + 当前 session/generation 的 PresentationEvent 才能通过首帧门；type-1 或旧会话回执不能通过。
8. 每个协议 crate 可独立编译，core/session/renderer/UI 不反向依赖具体协议；`cargo metadata` 架构检查拒绝表外依赖边。

### Windows 手动/真机门

- 单进程、单窗口完成登录页到远程桌面切换。
- 连接期间 UI 持续响应，取消与关闭不冻结。
- 首个真实画面前不显示黑屏成功状态。
- Apple HPSS/MVS 画面、颜色、增量和输入达到当前 `e2d1741` 真机行为等价。
- 窗口外鼠标、失焦键盘和断开后不再向远端发送输入。
- resize 不接受 stale generation；GPU Surface 恢复不破坏远端纹理。
- 空闲时没有固定 60 Hz 全帧 CPU 缩放或上传。
- RFB 和 RDP 在各自阶段只对对应系统原生服务端验证，不以其他协议成功代替。
- release、ZIP、MSI 在干净 Windows 环境完成安装、启动和卸载验证。

## 主要风险与控制

| 风险 | 控制 |
| --- | --- |
| 把当前 `hpss_viewer.rs` 原样搬进 winit host | 先拆 session/adapter/frame contract，host 禁止拥有 socket/crypto |
| 协议失败触发另一协议并污染状态 | 连接前解析唯一 protocol_id；无会话内 fallback |
| 旧 generation patch 写入新纹理 | 所有 Reset/Damage/FrameBoundary 强制 generation/revision 检查 |
| 增量队列溢出导致永久花屏 | canonical surface 生成最新 full snapshot |
| BGRX alpha=0 或重复 sRGB 转换导致黑屏/偏色 | 显式 PixelFormat、shader alpha=1、单次色彩转换测试 |
| Apple 与 RFB 因共享基础结构再次混合 | 只共享无状态 `frd-wire-rfb`；认证、fallback 和 session 分属各 adapter |
| 音频/硬解把平台代码带回协议 crate | 只通过 `frd-media-api` 发布媒体；设备与硬解 backend 在平台侧 |
| Surface 比原生窗口活得更久 | compositor 持有 owned `PresentationSurfaceLease` 并按固定顺序 detach |
| egui 和 remote pass 各自 acquire/present | compositor 独占 swapchain、encoder、submit 和 present |
| UI 线程执行连接、解码或 join | coordinator/adapter 后台运行，EventLoopProxy 仅唤醒 |
| 新 winit host 再次成为巨型文件 | shell、ui-model、egui views、renderer、platform services 分 crate/模块 |
| HarmonyOS NEXT 被错误当作 winit 已支持平台 | 独立 ArkUI/XComponent host，正式支持前完成真机 POC |
| 一次同时重写 renderer 和 MVS decoder 难以定位回归 | W2/W3 只改变边界和呈现，decoder 优化延后 |
| 历史跨平台 worktree 覆盖当前协议修复 | 仅在 `main@e2d1741` 上选择性迁移概念，不合并历史协议代码 |

## 完成定义

Windows-first 架构完成要求：

1. Windows 产品使用一个 winit 窗口、egui 控件层和 wgpu 远程画面 pass。
2. Apple、RFB、RDP 是三个独立 adapter；除无状态 wire/media API 外，失败、状态、buffer、认证和 crypto 互不共享。
3. 产品 UI、session 和 renderer 不导入具体协议实现，平台 shell 不解析协议。
4. Apple HPSS/MVS 达到当前真机画面与输入等价，RFB/RDP 分别通过其原生服务端验证。
5. minifb 和 Flutter 相关产品代码、依赖、构建脚本、测试和设计残留从当前实现中清理；协议实验 CLI 保持 headless。
6. Windows release、ZIP 和 MSI 可在干净环境安装、运行和卸载。
7. single-instance guard 和单会话槽阻止第二个产品窗口/会话并发操作同一远端。
8. 后续平台只需实现 shell/platform/presentation 接口，不需要复制或修改三个协议 adapter。
9. 未完成的 Android/HarmonyOS NEXT 平台验证明确标记为后续工作，不作为已经交付的能力宣传。
