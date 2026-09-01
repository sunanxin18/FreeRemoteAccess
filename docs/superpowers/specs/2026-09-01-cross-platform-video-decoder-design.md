# 跨平台统一视频能力查询与解码架构

日期：2026-09-01

状态：待用户书面审阅

首选方案：统一媒体契约、原生硬解优先、动态链接 LGPL FFmpeg 兜底

## 1. 目标

FreeRemoteDesk 同时面向多种原生远程桌面服务端和多个客户端平台。视频协议、解码器、
平台 API 与渲染器必须保持独立，使 Apple High Performance、RDP 和未来其他协议能够
复用同一套能力选择与解码运行时，而不把 Windows、macOS、Linux、Android 或
HarmonyOS 的实现细节写进协议 crate。

本设计建立以下统一能力：

1. 按 codec、profile、色度、位深、尺寸和输出格式精确查询解码能力；
2. 以同一接口创建、驱动、刷新和重置视频解码器；
3. 精确支持时优先选择平台原生硬解；
4. 原生后端不支持时自动选择固定版本、动态链接的 LGPL FFmpeg 后端；
5. 解码结果进入协议无关的帧发布与 wgpu 渲染路径；
6. decoder、surface、输入映射和动态分辨率使用同一 display generation；
7. 不支持或失败必须显式可诊断，不能回退到另一种远程桌面协议身份。

## 2. 已确认边界

- 当前 Apple High Performance 真机流是 8-bit HEVC Range Extensions、4:4:4、
  1920×1080；不能用只支持 HEVC Main/Main10 4:2:0 的后端冒充兼容。
- Apple HEVC RTP single/AP/FU、访问单元组装、参数集缓存、SPS 门禁和有界重排属于
  `frd-protocol-apple`；平台解码器不得重新解释 Apple RTP 或 SRTP。
- RDP、Apple Standard/MVS 和 Apple High Performance 是独立协议运行时。某个视频
  backend 不可用时，不得静默切换协议、连接模式或服务端身份。
- `wgpu` 是渲染与颜色转换层，不是通用 HEVC/H.264 解码器。
- 第一阶段不声称跨 API GPU surface 零拷贝已经可用。GPU 外部纹理导入必须在每个平台
  单独完成生命周期、同步和性能 POC 后扩展。

## 3. 方案选择

### 3.1 采用：统一 API + 原生硬解 + FFmpeg 兜底

协议输出统一的 `VideoStreamConfig` 和 `EncodedVideoAccessUnit`。组合根注册当前平台可用
的 decoder factory；registry 根据精确能力查询选择后端。原生硬解是首选，FFmpeg 可提供
其自身确认可用的硬件路径和软件路径，软件路径是所有官方客户端平台的最终兜底。

该方案同时保留平台性能控制、统一错误语义、确定性 fallback 和跨协议复用。

### 3.2 未采用：所有平台只通过 FFmpeg

该方案开发较快，但会把能力枚举、设备选择、零拷贝和平台错误诊断交给 FFmpeg 抽象，
无法为 Windows DirectX、macOS VideoToolbox 等平台路径提供足够精确的产品状态。

### 3.3 未采用：每个平台独立定义媒体接口

该方案会复制 profile 判断、队列、generation、错误和渲染衔接逻辑，并使协议 crate 逐步
依赖平台分支，不符合当前多服务端、多客户端的产品边界。

## 4. Crate 与所有权

| 组件 | 所有权 | 禁止依赖 |
|---|---|---|
| `frd-media-api` | 视频类型、能力查询、decoder factory/instance、registry 选择结果 | Apple、RDP、DirectX、FFmpeg、wgpu |
| `frd-protocol-apple` | SRTP、HEVC RTP、AU、参数集、Apple timestamp/generation | DirectX、FFmpeg、平台窗口、wgpu |
| `frd-protocol-rdp` | RDP graphics/codec 协商及 encoded AU 适配 | Apple、具体平台 decoder |
| `frd-platform-windows` | D3D12/DXVA/Media Foundation 能力探针与后续 Windows native backend | Apple/RDP wire grammar |
| 未来平台 crate | VideoToolbox、VA-API、MediaCodec、HarmonyOS native codec adapter | 协议 wire grammar |
| `frd-video-ffmpeg` | 动态库加载、FFmpeg 能力查询、软件/受支持硬件解码 | Apple/RDP wire grammar、窗口 UI |
| `frd-shell-desktop` | bounded decoder worker、backend registry 组合、帧转交 | 协议私有载荷、decoder 私有句柄 |
| `frd-compositor-wgpu` | CPU plane 上传、颜色转换、缩放和 present | SRTP、RTP、codec parser |

`frd-platform-api` 继续只承载通用平台服务。视频是媒体领域能力，接口位于
`frd-media-api`，避免把通用凭据、存储和窗口平台接口与 codec 状态耦合。

## 5. 中立媒体类型

当前 `MediaFrame::EncodedVideo { timestamp_us, bytes }` 信息不足，不能表达精确能力查询、
generation、timestamp timescale、参数集、profile 或 bitstream layout。它将迁移为以下概念。

### 5.1 视频流配置

```rust
pub struct VideoStreamConfig {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
    pub codec: VideoCodec,
    pub profile: VideoProfile,
    pub chroma: ChromaFormat,
    pub bit_depth: u8,
    pub coded_size: PixelSize,
    pub visible_rect: PixelRect,
    pub time_base: VideoTimeBase,
    pub bitstream_format: VideoBitstreamFormat,
    pub colorimetry: VideoColorimetry,
    pub range: VideoRange,
    pub chroma_location: ChromaLocation,
    pub parameter_sets: VideoParameterSets,
}
```

`VideoProfile` 必须能表达标准 profile 和证据不足时的 codec-specific profile descriptor；
未知值不能被压缩成 Main/Main10。Apple 已确认的 SPS 只有通过严格 parser 后才能形成
`HevcMain4448` 查询。

### 5.2 编码访问单元

```rust
pub struct EncodedVideoAccessUnit {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
    pub timestamp: VideoTimestamp,
    pub random_access: bool,
    pub bytes: Box<[u8]>,
}
```

`VideoTimestamp` 保存原始 ticks 与非零 timescale。协议 adapter 不得先假定 90 kHz 或把
无法证明的 clock 换算为微秒。AU 与参数集均有显式字节上限。

### 5.3 解码帧

第一阶段使用可移交所有权的 CPU planes：

```rust
pub struct DecodedVideoFrame {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
    pub timestamp: VideoTimestamp,
    pub coded_size: PixelSize,
    pub visible_rect: PixelRect,
    pub format: VideoPixelFormat,
    pub planes: Box<[VideoPlane]>,
}
```

每个 plane 带 stride、宽高和有界 buffer。FFmpeg Main444 输出保持 Y/U/V planes，由 wgpu
shader 完成 YUV 到 RGB 和缩放；禁止在 decoder worker 额外生成整帧 BGRA 副本。

未来 native GPU surface 作为独立、版本化的 `NativeVideoFrame` 扩展，不通过 `Any` 或裸
指针塞入第一阶段 API。扩展必须同时定义资源所有权、fence/synchronization、设备一致性、
丢帧释放和 compositor import 失败语义。

## 6. 能力查询

```rust
pub struct VideoDecodeQuery {
    pub codec: VideoCodec,
    pub profile: VideoProfile,
    pub chroma: ChromaFormat,
    pub bit_depth: u8,
    pub coded_size: PixelSize,
    pub frame_rate: Option<VideoRational>,
    pub preferred_outputs: Box<[VideoPixelFormat]>,
}

pub enum VideoDecodeSupport {
    HardwareExact(VideoDecodeCapability),
    SoftwareExact(VideoDecodeCapability),
    Unsupported(VideoUnsupportedReason),
}
```

能力结果不是 codec 名称级 `bool`。backend 只有在 profile、色度、位深、尺寸、输出格式和
驱动查询同时匹配时才能返回 `Exact`。能力结果还记录 backend id、adapter identity、输出
格式、是否需要 bitstream 转换和安全的最大尺寸，但不包含平台原生句柄。

Windows 探针使用 D3D12 Video 的 profile 枚举和 `CheckFeatureSupport` 查询实际 adapter；
标准 API 没有 Main444 profile 时返回明确的 `ProfileUnavailable`，不能因为存在 HEVC Main
或 Main10 GUID 就接受 Apple Main444。

## 7. Decoder 契约

```rust
pub trait VideoDecoderFactory: Send + Sync {
    fn backend_id(&self) -> VideoBackendId;
    fn query(&self, query: &VideoDecodeQuery) -> VideoDecodeSupport;
    fn create(
        &self,
        config: &VideoStreamConfig,
    ) -> Result<Box<dyn VideoDecoder>, VideoDecodeError>;
}

pub trait VideoDecoder: Send {
    fn submit(
        &mut self,
        access_unit: EncodedVideoAccessUnit,
    ) -> Result<DecodeOutcome, VideoDecodeError>;
    fn flush(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError>;
    fn reset(&mut self, generation: u64) -> Result<(), VideoDecodeError>;
}
```

`submit` 可返回 `NeedMoreData`、一帧或有限多帧。decoder 必须拒绝 stale identity、stale
generation、超预算 AU 和与已创建配置不一致的 bitstream。

decoder reset、旧帧释放、surface generation 和输入坐标映射必须由同一会话 generation
迁移驱动。不能把旧 decoder 的 reference frames 带入新尺寸或新 backend。

## 8. Registry 与 fallback 策略

组合根按以下优先级注册 backend：

1. 平台原生 `HardwareExact`；
2. FFmpeg 报告的 `HardwareExact`；
3. FFmpeg `SoftwareExact`；
4. 无精确 backend 时返回 `Unsupported`。

同一优先级按平台配置中的稳定顺序选择，不能依赖枚举偶然顺序。registry 返回包含所有
候选结果和最终选择的非敏感诊断记录，供状态 UI 与验证日志使用。

fallback 只在 decoder 创建前发生。第一阶段中，一旦某 backend 已提交首个真实帧，运行时
解码错误会终止该 generation 的视频并报告明确错误，不静默改用 FFmpeg、Standard、MVS
或另一协议。后续若增加关键帧重建 fallback，必须作为独立规格，要求新 decoder、参数集、
random-access AU 和显式状态事件。

## 9. 运行时与背压

- 每个视频 stream 有独立 decoder worker；网络 reader 只做协议解析和有界 AU 发布。
- AU 队列有固定包数和字节预算，不允许无限增长。
- 队列饱和时保持最新 random-access 恢复机会，丢弃策略必须显式计数；不得阻塞协议控制、
  音频、输入或断开流程。
- 帧队列保持很小的 latest-frame 语义。旧 generation 和已经被更新帧替代的帧及时释放。
- UI/winit 线程不执行 codec decode、动态库加载或设备能力查询。
- 关闭顺序为：停止接收新 AU、取消 worker、flush/close decoder、释放 queued frames、释放
  backend 设备状态，最后释放 renderer surface。

## 10. wgpu 衔接

第一阶段 compositor 为 Y、U、V plane 分别维护纹理，按有效 visible rect 上传并在 shader
中完成：

1. limited/full range 与 matrix 明确的 YUV 到线性 RGB；
2. chroma sampling 对应的坐标映射；
3. 可见裁剪；
4. 远端比例保持与窗口缩放；
5. 输出到现有 swapchain 色彩空间。

颜色参数由协议 adapter 从已验证的 bitstream/VUI 或协议元数据规范化进
`VideoStreamConfig`，不能由平台 backend 猜测。若没有足够元数据，使用一个明确、有测试
的产品默认值并在诊断中标记，而不是静默沿用上一 stream。

## 11. 平台路线

| 客户端平台 | 原生首选 | 兜底 | 首次门禁 |
|---|---|---|---|
| Windows | D3D12 Video/DXVA 或 Media Foundation 的精确硬解能力 | FFmpeg software；FFmpeg 确认的硬解可处于中间优先级 | 枚举真实 adapter profile；Apple Main444 不得误判 |
| macOS | VideoToolbox | FFmpeg software | profile/chroma/bit depth 真机 POC |
| Linux | VA-API 等发行版可用后端 | FFmpeg software | 驱动与显示栈矩阵 POC |
| Android | MediaCodec | FFmpeg software | codec list + surface 生命周期 POC |
| HarmonyOS | 平台 native codec adapter | FFmpeg software（工具链与许可 POC 后） | ArkTS/ArkUI、NDK、硬解 surface POC |

Android 与 HarmonyOS 不进入 Windows 第一阶段交付，但统一 API 不允许加入 Windows-only
类型，从而保证后续 backend 可实现而无需修改协议 adapter。

## 12. FFmpeg 安全、加载与许可

- 使用固定版本、动态链接的 LGPL build；关闭 GPL 和 nonfree 组件。
- 只构建交付所需 demux-free codec、parser、pixel-format 和必要硬件接口。
- Windows DLL 及其他平台动态库从应用安装目录的绝对、受控路径加载，不依赖当前目录或
  可被普通环境变量劫持的搜索顺序。
- 打包 LICENSE、构建参数、版本、修改说明及对应源代码获取方式。
- capability query 在加载失败、版本不匹配或符号缺失时返回 `BackendUnavailable`，不能导致
  整个应用启动失败；只有选择了需要视频的协议模式且无其他 backend 时才阻止连接。
- 所有 bitstream、extradata、plane size、stride、帧数量和缓存均在 Rust 边界先做 checked
  arithmetic 与资源上限验证。FFmpeg 错误文本不得携带网络地址、凭据或原始帧内容。

## 13. 协议接入

### 13.1 Apple High Performance

Apple runtime 将已认证视频 RTP 送入现有 reorder/AU assembler。完整 AU、严格 SPS-derived
配置和参数集进入统一 decoder worker。只有当前 generation 的第一个真实
`DecodedVideoFrame` 成功提交到 surface 后，产品才发布 High Performance 视频 Ready。

Apple Standard/MVS 使用其独立 pixel decoder 和 surface 路径，不通过该 HEVC registry，
也不作为失败 fallback。

### 13.2 RDP

当前 IronRDP 已解码像素路径保持不变。只有 RDP 协商获得经过验证的 encoded video AU
输出时才适配统一接口。Apple 接入不能改变 RDP capability、认证、surface 或输入状态。

### 13.3 RFB/VNC

Raw、Hextile、ZRLE、Tight 等像素/矩形编码继续走各自 CPU decoder。只有真正的视频 codec
扩展才进入统一视频接口，不能为了“统一”强制所有矩形更新绕过视频 decoder。

## 14. 状态与错误

至少区分以下稳定错误：

- backend unavailable；
- exact profile/chroma/bit-depth unsupported；
- output format unsupported；
- decoder creation failed；
- malformed or over-budget access unit；
- stale stream/generation；
- decode failed before first frame；
- decode failed after first frame；
- decoded frame layout invalid；
- frame publication failed。

能力不足不是认证失败或网络失败。UI 最终显示简体中文状态，但核心错误码保持稳定、可测试且
不包含动态库内部文本。未完成的 High Performance 模式不注册到产品选择器，避免以黑屏
冒充连接成功。

## 15. 实施阶段

### 阶段 1：统一契约与选择器

- 扩展 `frd-media-api`；
- 实现纯 Rust capability matching 和 registry；
- 用 fake backend 锁定精确匹配、优先级和 fallback；
- 迁移现有 `EncodedVideo` 测试调用，不改变现有音频和 pixel surface 行为。

### 阶段 2：Windows 能力探针

- 在 `frd-platform-windows` 实现只读 D3D12 Video profile/format/support 查询；
- 暴露开发诊断入口和结构化结果；
- 在当前 Windows GPU 上记录真实输出，不据 GPU 名称推断能力。

### 阶段 3：FFmpeg 兜底

- 新建 `frd-video-ffmpeg`；
- 实现固定 LGPL 动态库加载和 exact capability query；
- 解码 deterministic HEVC Main444 fixture 到 YUV444 planes；
- 验证加载失败只使 backend unavailable。

### 阶段 4：wgpu planar frame

- 实现 YUV444 三平面上传和 shader；
- 验证颜色、裁剪、比例、DPI、resize 和 stale generation；
- 不增加 CPU 整帧 BGRA 转换。

### 阶段 5：Apple HP 产品接入

- AU assembler 接统一 decoder worker；
- 真机确认首帧、持续更新、输入、断开和实体显示状态；
- 完成前保持产品 HP 模式未注册，不影响现有路径。

### 阶段 6：其他平台与可选零拷贝

- 按 Windows、macOS、Linux、Android、HarmonyOS 分别完成 native backend POC；
- 只有 POC 证明资源导入、同步和释放正确且性能有收益后，增加 GPU-native frame contract。

## 16. 核心测试与验收

遵循项目“不过度实现测试”的规则，只保留核心协议和架构门禁：

1. exact profile/chroma/bit-depth mismatch 必须拒绝；
2. native hardware exact 优先于 FFmpeg；
3. native unsupported 自动选择 FFmpeg software exact；
4. 所有 backend unsupported 时返回稳定错误；
5. stale generation AU/decoded frame 不进入 surface；
6. Apple 捕获 SPS 形成 Main444 查询，Windows 标准 Main/Main10 不能匹配；
7. FFmpeg fixture 输出正确的 YUV444 尺寸、stride 和非空像素；
8. decoder 首帧提交前不发布 HP Ready；
9. Apple、RDP 和既有 MVS 回归测试继续通过；
10. release 包只从受控位置加载固定 FFmpeg 动态库并包含许可材料。

Windows 第一阶段端到端验收：

```text
Apple Main444 SPS/AU
  -> 统一能力查询
  -> Windows native backend 精确拒绝或接受
  -> 不接受时选择 FFmpeg
  -> 输出 YUV444 planes
  -> wgpu 正确显示 1920×1080
  -> 首帧后发布 Ready
  -> 不改变 MVS/RDP 会话
```

## 17. README 跟踪

每个阶段在同一变更中更新顶层 README：分别记录客户端平台、目标服务端、协议、能力探针、
解码实现、package 生成和真机互操作状态。编译、探针枚举、fixture 解码和真实远程桌面首帧
是不同证据，不能合并为一个“已支持”状态。
