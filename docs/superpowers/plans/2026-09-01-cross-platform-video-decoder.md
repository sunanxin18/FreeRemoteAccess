# Cross-Platform Video Decoder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不改变 Apple Standard/MVS、RDP 与 VNC 既有协议身份的前提下，建立跨平台统一视频能力查询、精确 backend 选择、FFmpeg 8.1.2 Main444 软件兜底、YUV444 wgpu 渲染，并将 Apple High Performance 的已认证 HEVC AU 接入首帧可见的产品路径。

**Architecture:** `frd-media-api` 只定义协议无关的视频配置、访问单元、解码帧、能力与 factory 契约；各协议 crate 只负责把私有 wire 数据归一化。组合根按“可创建的原生精确硬解、FFmpeg 精确硬解、FFmpeg 精确软解”稳定排序。第一阶段使用拥有所有权的 CPU Y/U/V planes，`frd-render-wgpu` 完成三平面上传和 YUV→RGB；运行时首次真实 present 前不发布 High Performance Ready。FFmpeg 通过受控绝对路径加载客户端解码插件，缺失或 ABI 不匹配只禁用该 backend，不阻止应用启动。

**Tech Stack:** Rust 2021、wgpu 30.0.1、winit 0.30.13、Windows crate 0.62.2、libloading 0.8.9、FFmpeg 8.1.2（LGPLv2.1+、禁用 GPL/nonfree、动态链接）、现有 Apple SRTP/HEVC RTP/AU 组件。

**Spec:** `docs/superpowers/specs/2026-09-01-cross-platform-video-decoder-design.md`

## Global Constraints

- 所有任务在 `codex/mac-baseline-rdp-integration` 的隔离 worktree 中完成；开始每个任务前确认没有误触用户当前未提交的 Apple HP WIP。
- 不把 `README.md`、`crates/frd-protocol-apple/**` 或 `src/main.rs` 的现有脏改动纳入无关提交；需要修改这些文件的任务必须先以当前 WIP 为基线审阅差异并只暂存该任务的精确 hunk。
- Apple、RDP、VNC 运行时互不降级。decoder backend fallback 只允许在 decoder 创建前发生，首帧后失败必须终止当前视频 generation 并输出稳定错误。
- 不在协议 crate 引入 DirectX、FFmpeg、wgpu 或平台窗口 API；不在 renderer 中解析 RTP、SRTP、HEVC SPS 或 RDP graphics PDU。
- 仅实现核心架构门禁和确定性 fixture，不扩张为穷举 codec、驱动或 UI 测试矩阵。
- 所有长度、stride、plane、AU、队列字节数和尺寸计算使用 checked arithmetic，并遵守规格中的有界资源策略。
- 每个任务先写最小失败测试，再实现，再运行聚焦测试；每个任务独立提交，使用 Conventional Commit。
- 当前正式交付平台仍为 Windows。macOS、Linux、Android 与 HarmonyOS 的 native backend 只保留可实现接口和 README 状态，不在本计划伪造编译或真机证明。
- FFmpeg 固定使用官方 8.1.2 源码；构建参数禁止 `--enable-gpl` 与 `--enable-nonfree`，打包必须同时提供对应源代码位置、构建参数、修改说明和 LGPL 文本。

---

## Task 1: 建立中立视频类型并迁移旧媒体枚举

**Files:**

- Create: `crates/frd-media-api/src/video.rs`
- Modify: `crates/frd-media-api/src/lib.rs`
- Modify: `crates/frd-protocol-api/src/lib.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Modify: `tools/frd-legacy-minifb-lab/src/main.rs`
- Test: `crates/frd-media-api/src/video.rs`
- Test: `crates/frd-protocol-api/src/lib.rs`

- [ ] **Step 1: 写中立类型的失败测试**

在 `video.rs` 中先写测试，锁定非零 timebase、可见区域、plane stride/长度和 generation：

```rust
#[test]
fn stream_config_rejects_visible_rect_outside_coded_size() {
    let result = VideoStreamConfig::try_new(test_config_with_visible_rect(PixelRect {
        x: 0,
        y: 0,
        width: 1921,
        height: 1080,
    }));
    assert_eq!(result, Err(VideoContractError::VisibleRectOutOfBounds));
}

#[test]
fn plane_rejects_short_buffer_for_stride_and_height() {
    let result = VideoPlane::try_new(1920, 1080, 1920, vec![0; 1920 * 1079].into());
    assert_eq!(result, Err(VideoContractError::PlaneBufferTooShort));
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p frd-media-api video::tests -- --nocapture`

Expected: FAIL，因为 `VideoStreamConfig`、`VideoPlane` 和稳定错误尚不存在。

- [ ] **Step 3: 实现最小中立类型**

实现并从 `lib.rs` 导出以下 API；`VideoStreamIdentity` 使用 `SessionId + stream_id`，不使用协议枚举：

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VideoStreamIdentity {
    pub session_id: SessionId,
    pub stream_id: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoTimestamp {
    pub ticks: u64,
    pub timescale: NonZeroU32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedVideoAccessUnit {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
    pub timestamp: VideoTimestamp,
    pub random_access: bool,
    pub bytes: Box<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaFrame {
    Pcm { sample_rate_hz: u32, channels: u8, samples: Box<[i16]> },
    VideoConfig(VideoStreamConfig),
    EncodedVideo(EncodedVideoAccessUnit),
}
```

同时定义 `VideoCodec::{H264, Hevc}`、`VideoProfile::{H264Baseline, H264Main, H264High, HevcMain, HevcMain10, HevcMain4448, CodecSpecific { codec, profile_idc }}`、`ChromaFormat`、`VideoBitstreamFormat`、`VideoColorimetry`、`VideoRange`、`ChromaLocation`、`VideoParameterSets`、`VideoPixelFormat::{Yuv420P8, Yuv444P8, Nv12, P010}`、`DecodedVideoFrame` 和 `VideoPlane`。参数集与 AU 上限分别固定为 1 MiB 和 16 MiB。

- [ ] **Step 4: 迁移旧调用点**

将现有 `MediaFrame::EncodedVideo { timestamp_us, bytes }` 测试载荷迁移为 `MediaFrame::EncodedVideo(test_access_unit(...))`；shell 与 legacy lab 在 decoder worker 尚未接入前显式忽略 `VideoConfig`/`EncodedVideo`，PCM 行为保持不变。

- [ ] **Step 5: 运行 GREEN 与回归**

Run:

```powershell
cargo test -p frd-media-api
cargo test -p frd-protocol-api
cargo test -p frd-shell-desktop media
cargo test -p frd-protocol-rdp audio
```

Expected: 全部 PASS；现有 PCM 测试未改变。

- [ ] **Step 6: 提交**

```powershell
git add crates/frd-media-api/src/video.rs crates/frd-media-api/src/lib.rs crates/frd-protocol-api/src/lib.rs crates/frd-shell-desktop/src/application.rs tools/frd-legacy-minifb-lab/src/main.rs
git commit -m "feat: define protocol-neutral video media types"
```

---

## Task 2: 实现精确能力查询、factory 与稳定 registry

**Files:**

- Create: `crates/frd-media-api/src/decoder.rs`
- Create: `crates/frd-media-api/src/registry.rs`
- Modify: `crates/frd-media-api/src/lib.rs`
- Test: `crates/frd-media-api/src/registry.rs`

- [ ] **Step 1: 写精确匹配和排序的失败测试**

覆盖四个核心门禁：Main10 不匹配 Main444、原生硬解优先、原生不支持时选择 FFmpeg software、全部不支持时保留候选诊断。

```rust
#[test]
fn hevc_main10_never_matches_main444_query() {
    let query = main444_query();
    let capability = hevc_main10_capability();
    assert!(!capability.matches_exactly(&query));
}

#[test]
fn registry_uses_stable_backend_tier_order() {
    let registry = VideoDecoderRegistry::new(vec![
        fake_factory("ffmpeg-software", SoftwareExact),
        fake_factory("windows-native", HardwareExact),
    ]);
    assert_eq!(registry.select(&main444_query()).unwrap().backend_id.as_str(), "windows-native");
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p frd-media-api registry::tests -- --nocapture`

Expected: FAIL，因为 registry 契约尚不存在。

- [ ] **Step 3: 实现契约与稳定错误**

```rust
pub trait VideoCapabilityProvider: Send + Sync {
    fn backend_id(&self) -> VideoBackendId;
    fn availability(&self) -> VideoBackendAvailability;
    fn query(&self, query: &VideoDecodeQuery) -> VideoDecodeSupport;
}

pub trait VideoDecoderFactory: VideoCapabilityProvider {
    fn create(&self, config: &VideoStreamConfig)
        -> Result<Box<dyn VideoDecoder>, VideoDecodeError>;
}

pub trait VideoDecoder: Send {
    fn submit(&mut self, access_unit: EncodedVideoAccessUnit)
        -> Result<DecodeOutcome, VideoDecodeError>;
    fn flush(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError>;
    fn reset(&mut self, generation: u64) -> Result<(), VideoDecodeError>;
}
```

`VideoBackendAvailability` 区分 `DecoderReady` 与 `ProbeOnly`。registry 只选择 `DecoderReady` factory，允许 Windows native 探针报告真实能力但在 native decoder 未实现前绝不被错误选中。`VideoDecodeErrorCode` 至少包含规格第 14 节的九类稳定错误，不包含第三方原始字符串。

- [ ] **Step 4: 实现 registry**

`VideoDecoderRegistry::select` 返回 `VideoDecoderSelection { backend_id, support, diagnostics }`。排序键明确为 `(tier, registration_index)`，其中 native `HardwareExact=0`、FFmpeg `HardwareExact=1`、FFmpeg `SoftwareExact=2`；`ProbeOnly` 只进入 diagnostics。

- [ ] **Step 5: 运行 GREEN**

Run: `cargo test -p frd-media-api`

Expected: PASS，且没有平台/协议依赖进入 `frd-media-api/Cargo.toml`。

- [ ] **Step 6: 提交**

```powershell
git add crates/frd-media-api/src/decoder.rs crates/frd-media-api/src/registry.rs crates/frd-media-api/src/lib.rs
git commit -m "feat: add exact video decoder registry"
```

---

## Task 3: 添加 Windows D3D12 Video 只读能力探针

**Files:**

- Modify: `crates/frd-platform-windows/Cargo.toml`
- Create: `crates/frd-platform-windows/src/video_capabilities.rs`
- Modify: `crates/frd-platform-windows/src/lib.rs`
- Create: `tools/frd-video-capabilities/Cargo.toml`
- Create: `tools/frd-video-capabilities/src/main.rs`
- Modify: `Cargo.toml`
- Test: `crates/frd-platform-windows/src/video_capabilities.rs`

- [ ] **Step 1: 写 profile 映射的失败测试**

测试只允许 Windows 已知 GUID 映射到 Main/Main10；未知 GUID 和 Main444 query 返回 `ProfileUnavailable`，GPU 名称不能作为能力判定依据。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p frd-platform-windows video_capabilities -- --nocapture`

Expected: FAIL，因为 Windows provider 尚不存在。

- [ ] **Step 3: 增加 Windows 0.62.2 COM 依赖**

在 Windows target dependencies 增加：

```toml
windows = { version = "=0.62.2", features = [
    "Win32_Foundation",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D12",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_System_Com",
] }
```

保留现有 `windows-sys`，不在本任务机械迁移其他平台服务。

- [ ] **Step 4: 实现只读 provider**

`WindowsVideoCapabilityProvider::probe()` 创建 DXGI factory，按 adapter preference 枚举非 software adapter，创建 D3D12 device 与 video device，枚举 decode profiles，并用 `CheckFeatureSupport` 验证 profile、格式、尺寸。输出只包含 adapter LUID 的脱敏十六进制、描述、支持结果和稳定错误码。

```rust
pub struct WindowsVideoCapabilityProvider { adapters: Box<[WindowsVideoAdapter]> }

impl VideoCapabilityProvider for WindowsVideoCapabilityProvider {
    fn availability(&self) -> VideoBackendAvailability {
        VideoBackendAvailability::ProbeOnly
    }
    // query 必须逐项匹配 profile/chroma/bit depth/size/output。
}
```

- [ ] **Step 5: 创建无 GUI 诊断工具**

`frd-video-capabilities` 默认输出人类可读摘要，`--json` 输出结构化查询结果；它不接收凭据、不连接远端、不进入正式 GUI binary。默认查询至少包括 H.264 High 4:2:0 8-bit、HEVC Main 4:2:0 8-bit、Main10 4:2:0 10-bit 和 Apple 已确认 Main444 4:4:4 8-bit。

- [ ] **Step 6: 运行 GREEN 和本机探针**

Run:

```powershell
cargo test -p frd-platform-windows video_capabilities
cargo run -p frd-video-capabilities -- --json
```

Expected: 测试 PASS；JSON 有至少一个 adapter 或稳定的 `adapter_unavailable`；Main444 只能由真实精确查询确认，不能因 Main/Main10 存在而显示支持。

- [ ] **Step 7: 保存非敏感探针证据并提交**

将一次本机输出写入 `docs/validation/windows-video-capabilities-20260901.json`，其中不含用户名、主机名、凭据或远端地址。

```powershell
git add Cargo.toml crates/frd-platform-windows tools/frd-video-capabilities docs/validation/windows-video-capabilities-20260901.json
git commit -m "feat: probe Windows video decode capabilities"
```

---

## Task 4: 建立可选加载的 FFmpeg 8.1.2 解码插件边界

**Files:**

- Create: `crates/frd-video-ffmpeg/Cargo.toml`
- Create: `crates/frd-video-ffmpeg/src/lib.rs`
- Create: `crates/frd-video-ffmpeg/src/abi.rs`
- Create: `crates/frd-video-ffmpeg/src/loader.rs`
- Create: `crates/frd-video-ffmpeg-plugin/Cargo.toml`
- Create: `crates/frd-video-ffmpeg-plugin/src/lib.rs`
- Create: `crates/frd-video-ffmpeg-plugin/src/decoder.rs`
- Modify: `Cargo.toml`
- Test: `crates/frd-video-ffmpeg/src/loader.rs`

- [ ] **Step 1: 写插件缺失和 ABI 错误的失败测试**

```rust
#[test]
fn missing_plugin_is_backend_unavailable_not_process_failure() {
    let result = FfmpegBackend::load_from(test_dir("missing"));
    assert_eq!(result.unwrap_err().code(), VideoDecodeErrorCode::BackendUnavailable);
}

#[test]
fn incompatible_plugin_abi_is_rejected_before_factory_registration() {
    let result = load_fake_plugin_with_abi(FRD_FFMPEG_ABI_VERSION + 1);
    assert_eq!(result.unwrap_err().code(), VideoDecodeErrorCode::BackendVersionMismatch);
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p frd-video-ffmpeg loader -- --nocapture`

Expected: FAIL，因为 loader crate 尚不存在。

- [ ] **Step 3: 定义小型稳定 C ABI**

`frd-video-ffmpeg` 是产品侧安全包装与绝对路径 loader；`frd-video-ffmpeg-plugin` 是 `cdylib`，使用 `ffmpeg-the-third = { version = "=5.0.0", default-features = false, features = ["codec"] }` 对接 FFmpeg 8.1.x。ABI 不暴露 Rust trait object、`Vec`、`String` 或 FFmpeg struct：

```rust
pub const FRD_FFMPEG_ABI_VERSION: u32 = 1;

#[repr(C)]
pub struct FrdFfmpegApiV1 {
    pub abi_version: u32,
    pub avcodec_major: u32,
    pub create_decoder: unsafe extern "C" fn(*const FrdVideoConfig, *mut FrdDecoderHandle) -> FrdStatus,
    pub submit: unsafe extern "C" fn(FrdDecoderHandle, *const u8, usize, i64, u32) -> FrdStatus,
    pub receive: unsafe extern "C" fn(FrdDecoderHandle, *mut FrdDecodedFrame) -> FrdStatus,
    pub flush: unsafe extern "C" fn(FrdDecoderHandle) -> FrdStatus,
    pub destroy: unsafe extern "C" fn(FrdDecoderHandle),
}
```

所有输出 buffer 通过插件提供的 release callback 释放，确保跨 CRT 边界不混用 allocator。

- [ ] **Step 4: 实现受控加载**

产品只从 `application_dir/codecs/ffmpeg-8.1.2/<platform>/` 的 canonical 绝对路径加载。Windows 先用 `LoadLibraryExW` 和 `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS` 加载官方名称 DLL，再加载 `freeremotedesk_ffmpeg.dll`；Unix loader 使用 `RTLD_NOW | RTLD_LOCAL`。拒绝相对路径、`..`、环境变量覆盖和符号缺失。

- [ ] **Step 5: 实现 plugin factory 最小 query**

plugin 调用 `avcodec_version` 校验 libavcodec major 62，确认内建 HEVC decoder 与输出 pixel format。第一阶段只注册 `SoftwareExact`；硬件设备类型尚未完成 exact 验证时不得返回 `HardwareExact`。

- [ ] **Step 6: 运行 GREEN**

Run:

```powershell
cargo test -p frd-video-ffmpeg
cargo check -p frd-video-ffmpeg-plugin
```

Expected: loader 测试 PASS；在未配置 FFmpeg 开发库的机器上，主 workspace 仍能 build，只有显式构建 plugin 时报告清晰的 native dependency 缺失。

- [ ] **Step 7: 提交**

```powershell
git add Cargo.toml crates/frd-video-ffmpeg crates/frd-video-ffmpeg-plugin
git commit -m "feat: add optional FFmpeg decoder plugin boundary"
```

---

## Task 5: 构建固定 LGPL FFmpeg 并完成 Main444 fixture 解码

**Files:**

- Create: `third_party/ffmpeg/8.1.2/README.md`
- Create: `third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1`
- Create: `third_party/ffmpeg/8.1.2/configure-windows.txt`
- Create: `third_party/ffmpeg/8.1.2/changes.diff`
- Create: `tools/build-ffmpeg-windows.ps1`
- Modify: `crates/frd-video-ffmpeg-plugin/src/decoder.rs`
- Create: `crates/frd-video-ffmpeg-plugin/tests/main444_decode.rs`
- Create: `crates/frd-video-ffmpeg-plugin/tests/fixtures/apple-main444-idr.hevc`
- Create: `crates/frd-video-ffmpeg-plugin/tests/fixtures/apple-main444-idr.json`

- [ ] **Step 1: 固定来源与构建参数**

脚本下载 `https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz` 及 `.asc`，校验仓库中记录的 SHA-256 和 FFmpeg 发布签名，然后执行固定 configure。核心参数：

```text
--disable-static --enable-shared --disable-programs --disable-doc
--disable-everything --enable-decoder=hevc --enable-parser=hevc
--enable-protocol=file --disable-gpl --disable-nonfree
```

不得启用 x264/x265/fdk-aac 等外部库。`README.md` 记录官方源地址、确切版本、校验方式、构建命令与对应源代码分发位置。

- [ ] **Step 2: 写 Main444 fixture RED**

fixture 只包含已授权捕获中去标识化的一组 VPS/SPS/PPS + 单个 random-access AU；JSON 记录 codec/profile/chroma/bit-depth、coded/visible size 和期望 Y/U/V plane checksum，不包含 IP、用户名或时间戳。

Run: `cargo test -p frd-video-ffmpeg-plugin --test main444_decode -- --nocapture`

Expected: FAIL，因为 decoder 尚未完成 receive/copy/validation。

- [ ] **Step 3: 实现 software decode**

创建 HEVC decoder context，使用显式 extradata/Annex-B 输入；`send_packet`/`receive_frame` 处理 EAGAIN 和有限多帧。只接受当前阶段声明的 `AV_PIX_FMT_YUV444P`，将每个 plane 按 FFmpeg `linesize` 校验后复制到拥有所有权的 `VideoPlane`，不生成 BGRA。负 stride、超 8192×8192、总 plane 超 256 MiB 或不匹配 config 均返回稳定错误。

- [ ] **Step 4: 运行 GREEN**

Run:

```powershell
pwsh -File tools/build-ffmpeg-windows.ps1 -Configuration Release
cargo test -p frd-video-ffmpeg-plugin --test main444_decode -- --nocapture
cargo test -p frd-video-ffmpeg
```

Expected: fixture 输出 `Yuv444P8`，三个 plane 尺寸/stride/checksum 与 JSON 完全一致；删除 codec 目录后 loader 测试仍只返回 backend unavailable。

- [ ] **Step 5: 提交**

```powershell
git add third_party/ffmpeg/8.1.2 tools/build-ffmpeg-windows.ps1 crates/frd-video-ffmpeg-plugin
git commit -m "feat: decode HEVC Main444 with FFmpeg fallback"
```

---

## Task 6: 在 desktop shell 建立有界 decoder worker

**Files:**

- Create: `crates/frd-shell-desktop/src/video_decode_worker.rs`
- Modify: `crates/frd-shell-desktop/src/lib.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Modify: `crates/frd-shell-desktop/Cargo.toml`
- Test: `crates/frd-shell-desktop/src/video_decode_worker.rs`

- [ ] **Step 1: 写背压、generation 与首帧门禁 RED**

测试 64 个 AU / 32 MiB 双预算、stale generation 丢弃、首帧前失败与首帧后失败不同错误、latest-frame 队列只保留最新当前 generation 帧。

```rust
#[test]
fn ready_is_not_emitted_until_current_generation_frame_is_accepted() {
    let mut worker = harness_with_fake_decoder();
    worker.send_config(test_config(7)).unwrap();
    worker.send_access_unit(test_au(7)).unwrap();
    assert_eq!(worker.recv_event(), VideoWorkerEvent::FrameDecoded(test_frame(7)));
    assert!(!worker.is_ready());
    worker.confirm_presented(7).unwrap();
    assert!(worker.is_ready());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p frd-shell-desktop video_decode_worker -- --nocapture`

Expected: FAIL，因为 worker 尚不存在。

- [ ] **Step 3: 实现 worker**

`VideoDecodeWorker` 在线程内执行 registry select、plugin load、decoder create/submit/flush。network publisher 只使用 `try_send`；饱和时丢弃非 random-access AU，并保留最新 random-access 恢复点。线程事件为：

```rust
pub enum VideoWorkerEvent {
    BackendSelected(VideoDecoderDiagnostics),
    FrameDecoded(DecodedVideoFrame),
    DecodeFailed { generation: u64, code: VideoDecodeErrorCode, after_first_frame: bool },
    Stopped,
}
```

`application.rs` 只负责创建/关闭 worker 和把 frame 交给 renderer，不执行 codec 工作。现有音频接收循环拆成 `drain_audio_media`，避免视频解码阻塞 PCM。

- [ ] **Step 4: 运行 GREEN 与 shell 回归**

Run:

```powershell
cargo test -p frd-shell-desktop video_decode_worker
cargo test -p frd-shell-desktop
```

Expected: PASS；fatal 后直接输出错误并退出的既有行为不回归。

- [ ] **Step 5: 提交**

```powershell
git add crates/frd-shell-desktop/src/video_decode_worker.rs crates/frd-shell-desktop/src/lib.rs crates/frd-shell-desktop/src/application.rs crates/frd-shell-desktop/Cargo.toml
git commit -m "feat: add bounded desktop video decoder worker"
```

---

## Task 7: 为 wgpu renderer 增加 YUV444 三平面路径

**Files:**

- Create: `crates/frd-render-wgpu/src/video_texture.rs`
- Create: `crates/frd-render-wgpu/src/shaders/video_yuv444.wgsl`
- Modify: `crates/frd-render-wgpu/src/lib.rs`
- Modify: `crates/frd-compositor-wgpu/src/lib.rs`
- Modify: `crates/frd-compositor-wgpu/Cargo.toml`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Test: `crates/frd-render-wgpu/src/video_texture.rs`
- Test: `crates/frd-compositor-wgpu/src/lib.rs`

- [ ] **Step 1: 写 frame layout、颜色参数和 stale generation RED**

测试 visible rect 裁剪、Y/U/V stride 对齐、BT.709 limited/full range matrix 选择、旧 generation frame 不上传、不同尺寸重建纹理。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p frd-render-wgpu video_texture -- --nocapture`

Expected: FAIL，因为 video texture contract 尚不存在。

- [ ] **Step 3: 实现三平面纹理与 shader**

每个 plane 使用 `R8Unorm` texture；通过 `queue.write_texture` 按 plane stride 上传。uniform 至少包括 visible rect、coded size、Y offset/scale 和 3×3 matrix。BT.709 limited-range 的核心转换写成显式 WGSL，不从上一 stream 继承：

```wgsl
let y = (textureSample(y_tex, linear_sampler, uv).r - color.y_offset) * color.y_scale;
let u = textureSample(u_tex, linear_sampler, uv).r - 0.5;
let v = textureSample(v_tex, linear_sampler, uv).r - 0.5;
let rgb = color.yuv_to_rgb * vec3<f32>(y, u, v);
return vec4<f32>(clamp(rgb, vec3(0.0), vec3(1.0)), 1.0);
```

`RemoteRenderer` 保持原 pixel surface 路径，新增独立 `VideoRenderer`；二者共享 `GpuContext` 和 presentation viewport，但不能共享协议状态。

- [ ] **Step 4: 增加 GPU readback 核心测试**

用 4×4 确定性 YUV444 fixture 渲染到离屏纹理并 readback，允许每通道 ±1 量化误差；验证红、绿、蓝、灰和 visible rect 裁剪。只保留这一组核心颜色门禁，不扩张全色域测试。

- [ ] **Step 5: 运行 GREEN**

Run:

```powershell
cargo test -p frd-render-wgpu video_texture
cargo test -p frd-compositor-wgpu video
cargo test -p frd-shell-desktop
```

Expected: PASS；现有 BGRA pixel surface 路径测试不变。

- [ ] **Step 6: 提交**

```powershell
git add crates/frd-render-wgpu/src/video_texture.rs crates/frd-render-wgpu/src/shaders/video_yuv444.wgsl crates/frd-render-wgpu/src/lib.rs crates/frd-compositor-wgpu crates/frd-shell-desktop/src/application.rs
git commit -m "feat: render decoded YUV444 frames with wgpu"
```

---

## Task 8: 将 Apple High Performance AU 接入统一视频路径

**Files:**

- Modify: `crates/frd-protocol-apple/src/hevc_access_unit.rs`
- Modify: `crates/frd-protocol-apple/src/hevc_sps.rs`
- Create: `crates/frd-protocol-apple/src/high_performance_video.rs`
- Modify: `crates/frd-protocol-apple/src/lib.rs`
- Modify: `crates/frd-protocol-apple/src/session.rs`
- Modify: `crates/frd-protocol-apple/src/factory.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Test: `crates/frd-protocol-apple/src/high_performance_video.rs`
- Test: `crates/frd-shell-desktop/src/video_decode_worker.rs`

- [ ] **Step 1: 先审阅现有脏 WIP**

Run:

```powershell
git diff -- crates/frd-protocol-apple/src/hevc_access_unit.rs crates/frd-protocol-apple/src/hevc_sps.rs crates/frd-protocol-apple/src/session.rs crates/frd-protocol-apple/src/factory.rs
```

Expected: 明确哪些 HEVC RTP/AU/SPS 代码属于已验证 WIP；本任务在其上做最小 adapter，不重写或混入 MVS。

- [ ] **Step 2: 写 SPS→config 和 Ready 门禁 RED**

使用现有捕获 SPS fixture 断言生成 `HevcMain4448 / Cs444 / 8-bit / 1920×1080 / AnnexB`；Main/Main10 不能冒充匹配。再以 fake decoder/presenter 断言：认证、UDP、AU 完成都不能单独产生 Ready，只有 current generation 首帧 present confirmation 才能产生。

- [ ] **Step 3: 运行 RED**

Run:

```powershell
cargo test -p frd-protocol-apple high_performance_video -- --nocapture
cargo test -p frd-shell-desktop ready_is_not_emitted -- --nocapture
```

Expected: FAIL，因为协议 adapter 和产品确认链尚未接通。

- [ ] **Step 4: 实现协议 adapter**

`AppleHighPerformanceVideoAdapter` 接收 AU assembler 输出；首次严格 SPS 后先发布 `MediaFrame::VideoConfig`，随后发布同 identity/generation 的 `MediaFrame::EncodedVideo`。它不加载 decoder、不知道 Windows、不发布 surface、不调用 wgpu。

- [ ] **Step 5: 接通产品模式**

仅当以下链路完整时注册 Apple High Performance 选择项：

```text
authenticated SRTP video
-> bounded reorder/AU
-> strict SPS-derived config
-> registry selects DecoderReady backend
-> decoded current-generation frame
-> wgpu upload and successful present confirmation
-> AppleHighPerformanceVideoReady
```

任何一步失败显示对应稳定状态并断开该模式；禁止自动切换 `AppleTcpMvs`、Standard 或 RDP。Apple Standard/MVS factory identity 与 runtime 不改。

- [ ] **Step 6: 运行 GREEN 与协议隔离回归**

Run:

```powershell
cargo test -p frd-protocol-apple high_performance_video
cargo test -p frd-protocol-apple
cargo test -p frd-protocol-rdp
cargo test -p frd-shell-desktop
```

Expected: PASS；HP Ready 只在首帧 present 后出现；RDP/MVS 测试无行为变化。

- [ ] **Step 7: 精确暂存并提交**

先执行 `git diff --check`，再用 `git add -p` 只暂存本任务 hunk；确认 `git diff --cached --stat` 不包含无关实验或凭据。

```powershell
git commit -m "feat: connect Apple High Performance video pipeline"
```

---

## Task 9: Windows 打包、许可与平台状态矩阵

**Files:**

- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `apps/freeremotedesk-windows/build.rs`
- Modify: `.github/workflows/build-windows.yml`
- Create: `packaging/windows/ffmpeg-manifest.json`
- Create: `packaging/windows/licenses/FFmpeg-LGPL-2.1-or-later.txt`
- Create: `packaging/windows/licenses/FFmpeg-NOTICE.txt`
- Modify: `README.md`
- Test: `tools/verify-windows-package.ps1`

- [ ] **Step 1: 写 package verifier RED**

验证安装包 staging 目录必须包含版本化 codec 子目录、官方 DLL 名、plugin DLL、license、notice、source URL、configure 参数和 SHA-256 manifest；同时验证应用根目录和当前目录不存在第二份可优先加载的同名 DLL。

Run: `pwsh -File tools/verify-windows-package.ps1 -PackageRoot target/package-test`

Expected: FAIL，因为 package manifest 尚不存在。

- [ ] **Step 2: 集成 release 打包**

构建流程先产生 FFmpeg shared DLL 与 plugin，再复制到 `codecs/ffmpeg-8.1.2/windows-x86_64/`。manifest 记录每个文件 SHA-256 和 libavcodec major 62。安装/卸载只影响客户端安装目录，不在 Mac 服务端部署任何组件。

- [ ] **Step 3: 更新 README 平台矩阵**

分别记录：

- Windows client：能力探针 `已验证` 或 `受限验证`，附证据路径；
- FFmpeg Main444 fixture：`已验证`，明确仅离线 fixture；
- Apple HP 真机首帧：未完成前保持 `开发中`；
- macOS/Linux/Android/HarmonyOS native backend：`计划中`，不冒充 build 支持；
- RDP、Apple Standard/MVS 既有状态保持原证据，不因统一 decoder 改写。

- [ ] **Step 4: 运行 GREEN**

Run:

```powershell
cargo build --release -p freeremotedesk-windows
pwsh -File tools/verify-windows-package.ps1 -PackageRoot target/package-test
```

Expected: PASS；临时移走 codec 目录后 GUI 仍能启动并把 FFmpeg backend 标为 unavailable。

- [ ] **Step 5: 提交**

```powershell
git add apps/freeremotedesk-windows .github/workflows/build-windows.yml packaging/windows tools/verify-windows-package.ps1 README.md
git commit -m "build: package optional FFmpeg video backend"
```

---

## Task 10: 完成离线、Windows 本机与 Mac 真机验收

**Files:**

- Create: `docs/validation/cross-platform-video-decoder-20260901.md`
- Modify: `README.md`

- [ ] **Step 1: 运行格式与全量核心回归**

Run:

```powershell
cargo fmt --all -- --check
cargo test -p frd-media-api
cargo test -p frd-platform-windows
cargo test -p frd-video-ffmpeg
cargo test -p frd-video-ffmpeg-plugin
cargo test -p frd-render-wgpu
cargo test -p frd-compositor-wgpu
cargo test -p frd-shell-desktop
cargo test -p frd-protocol-apple
cargo test -p frd-protocol-rdp
cargo build --release -p freeremotedesk-windows
```

Expected: 全部 PASS；仅允许记录已存在且与本变更无关的 warning。

- [ ] **Step 2: Windows 本机受限验证**

验证 capability 工具、FFmpeg plugin 加载、Main444 fixture、YUV444 离屏颜色、GUI 无 codec 目录启动、GUI 有 codec 目录显示 backend 状态。记录命令、版本、GPU adapter、结果和限制，不记录用户名、主机名或凭据。

- [ ] **Step 3: Mac 真机验证（仅网络恢复后执行）**

从 `CREDENTIALS.local.md` 或 OS 安全凭据库读取目标，不把凭据写入命令行。验收：

1. Apple High Performance 认证成功；
2. 接收并认证视频 RTP，形成 Main444 config/AU；
3. registry 精确拒绝不支持的 native backend并选择 FFmpeg software；
4. 当前 generation 首帧正确显示后才 Ready；
5. 连续画面更新、鼠标、键盘、滚轮与断开正常；
6. 画面比例和颜色正确，无黑块、无旧 generation 帧；
7. Apple Standard/MVS 单独连接仍正常；
8. RDP 无真实 Windows server 时明确记录“未真机验证”，不伪造通过。

- [ ] **Step 4: 记录性能预算**

采集 60 秒窗口：AU queue 峰值、丢弃计数、decode p50/p95、decoded→present p50/p95、frame queue 峰值、CPU 与内存。验收门禁为队列有界、持续更新无单调积压；本任务不凭单次主观观感承诺固定毫秒 SLA。

- [ ] **Step 5: 更新证据文档与 README**

`docs/validation/cross-platform-video-decoder-20260901.md` 分开记录编译、fixture、本机 probe、package 和真机互操作。README 只把实际完成的格子提升为 `已验证`/`受限验证`；阻塞项保留确切原因。

- [ ] **Step 6: 最终差异审计**

Run:

```powershell
git diff --check
git status --short
git diff --name-only HEAD~10..HEAD
git grep -n -E "nswdxka|192\\.168\\.|FRD_PASSWORD=" -- . ":(exclude)CREDENTIALS.local.md"
```

Expected: 无 whitespace 错误、无真实凭据/IP、无 protocol→platform/renderer 反向依赖、无意外生成物。

- [ ] **Step 7: 提交验收记录**

```powershell
git add docs/validation/cross-platform-video-decoder-20260901.md README.md
git commit -m "docs: record unified video decoder validation"
```

---

## Completion Gate

- [ ] `frd-media-api` 不依赖 Apple、RDP、DirectX、FFmpeg 或 wgpu。
- [ ] Windows native probe 的 Main/Main10 不能匹配 Apple Main444。
- [ ] registry 只选择 `DecoderReady` factory，并在创建前完成确定性 fallback。
- [ ] FFmpeg 缺失不会阻止 GUI 启动；需要 HP 视频且无 exact backend 时才阻止该连接模式。
- [ ] FFmpeg Main444 fixture 输出三平面 YUV444，不产生 CPU BGRA 整帧副本。
- [ ] wgpu 路径按显式 colorimetry/range 渲染并拒绝 stale generation。
- [ ] Apple HP Ready 仅在 current-generation 首帧实际 present 后发布。
- [ ] Apple Standard/MVS、RDP 和 VNC 既有路径未被自动降级或混合。
- [ ] Windows 安装包包含固定 DLL、plugin、manifest、LGPL、构建参数和对应源代码说明。
- [ ] README 分开记录客户端平台、服务端平台、协议、编译、打包和真机证据。
