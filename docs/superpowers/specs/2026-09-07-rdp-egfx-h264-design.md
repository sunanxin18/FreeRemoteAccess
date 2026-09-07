# RDP EGFX H.264 解码设计

## 目标

在不破坏现有 Windows、macOS 和未来 Linux 客户端接口的前提下，为 RDP 增加
RDPGFX/EGFX AVC420（以及后续 AVC444、HEVC）路径。实现顺序固定为
AVC420 → RemoteFX 优化 → AVC444 → HEVC；HEVC 最后实现并完成真实互操作验证。
只有在客户端拥有可验证的解码器、服务端实际选择了对应编码、并且该平台/服务端
组合已经通过生产互操作门禁时，才允许启用对应路径；否则继续使用可用的回退路径。

## 当前事实

- `frd-protocol-rdp` 固定依赖 IronRDP 0.17.0。
- 没有 decoder provider 时，连接器只声明传统 Bitmap codec，`client_codecs_capabilities(&[])`
  默认产生 RemoteFX capability；精确 provider 注入后，IronRDP EGFX DVC seam 会设置
  图形通道 early capability bit，并注册 AVC420 adapter。没有 provider 的默认路径仍不
  广告 RDPGFX/AVC，保持 legacy fallback。
- 当前 RDP 解码器支持 Raw Bitmap、Interleaved RLE、RDP 6 Bitmap 和 RemoteFX。
- `frd-video-ffmpeg` 保留 HEVC Main444 8-bit / YUV444P8 的 Apple High Performance
  路径，并新增了默认关闭的 H.264 AVC420/YUV420P8 ABI、FFmpeg bridge 和 length-prefixed
  转换；EGFX handler 已能把已映射的 AVC420 RGBA 结果排入 generation-bound
  `SurfaceUpdate` 队列。Windows/macOS 组合根在固定 bundle 能力精确匹配时注入
  `Avc420DecoderProvider`，active session 会将 EGFX Reset/Damage/FrameBoundary 提交给
  `ProtocolRuntime`；bundle、decoder、DVC 或尺寸校验失败时仍回退到 Bitmap/RemoteFX。
- `frd-media-api` 的协议中立 decoder registry 已有精确 H.264 AVC420 profile/input
  contract；AVC444 profile、YUV444P8 FFmpeg capability slot 和独立 native entrypoint
  已通过离线合同测试。RDP AVC444 现在还包含 `RFX_AVC444_BITMAP_STREAM` envelope
  校验、V1/V2 YUV420/Chroma420 事务性双流重建和显式 `Avc444DecoderProvider` 构造边界，
  但生产组合根和服务器 wire/profile/live 门禁仍保持关闭。

## 不变量

- 不宣告没有可用解码器的 AVC420/AVC444 capability；能力广告必须与实际 decoder
  完全一致。
- RDP transport、EGFX PDU/DVC、codec decoder 和 `SurfaceUpdate` 分层；
  `frd-protocol-rdp` 不直接依赖 FFmpeg ABI。
- H.264 解码结果必须仍然发布为现有 `Reset`、`Damage`、`FrameBoundary`，窗口、
  输入、分辨率和 renderer 接口保持不变。
- 实现顺序和生产选择顺序必须分离。HEVC 虽然最后实现，但在 HEVC 通过对应平台
  与服务端的真实互操作验证后，生产选择器应优先选择 HEVC；不能仅凭编译、离线
  fixture 或本地显卡探针把 HEVC 标记为生产可用。
- 生产选择器的首选顺序为：已验证且已协商的 HEVC，其次是已验证的 AVC420，
  再按画面类型选择 AVC444 或 RemoteFX，最后回退到 Bitmap。每一级都必须满足
  精确 profile、像素格式、分辨率和 decoder backend 能力；失败时降到下一级。
- IronRDP 的协议状态机、能力协商和 generation/frame 状态保留可移植 Rust 实现。
  生产软件解码的 codec、颜色转换和像素拷贝热点必须使用目标架构的汇编或 SIMD；
  优先复用 FFmpeg 已验证的 x86/x86_64 与 AArch64/NEON 实现。Rust 标量路径只能
  作为参考实现、确定性测试 oracle 或显式 unsupported-CPU 回退，不能静默成为已
  支持架构的生产解码器。
- 每个目标架构必须独立完成编译门禁、运行时特性检查、逐像素一致性测试和基准
  记录；不能把 NASM-only 产物或另一架构的汇编路径当成 ARM64/x86 支持。
- x86/i686、x86_64/AMD64、AArch64 必须独立构建和验证；不能把 NASM-only 产物
  当成 ARM64 支持。
- 不把 Windows x86、Linux x86 或 Linux ARM64 标记为已支持，除非对应 Rust、
  FFmpeg runtime、加载器和 package gate 全部通过。

## 目标数据流

```text
RDP transport
  ├─ legacy Bitmap / RLE / RDP6 / RemoteFX
  │    └─ IronRDP ActiveStage
  │         └─ existing SurfaceUpdate path
  │
  └─ Graphics DVC (RDPGFX)
       └─ EGFX capability + PDU state machine
            └─ AVC420/AVC444 access unit
                 └─ VideoDecoderRegistry
                      ├─ platform hardware decoder when exact capability exists
                      ├─ FFmpeg H.264 software decoder
                      └─ no decoder: do not advertise AVC
                           └─ SurfaceUpdate
```

EGFX capabilities are observed from the actual server `CapabilitiesConfirm` and exposed
as non-secret session diagnostics. A server capability alone does not switch the renderer;
the switch requires a matching decoder, valid surface state, a decoded frame and a
generation-bound frame boundary.

## Decoder backends

The first correctness backend is FFmpeg software H.264. Its ABI must become codec/profile/
pixel-format aware and support RDPGFX's AVC length-prefixed NAL units, SPS/PPS lifecycle,
random-access reset and YUV420 output. The RDP adapter performs only the RDPGFX framing
conversion; the decoder owns H.264 reference state.

Platform hardware backends are separate follow-up work. Windows may use Media Foundation
or D3D12 Video, macOS may use VideoToolbox, and Linux may use VA-API/V4L2. They must be
selected by exact capability and never be inferred from OS or CPU name alone. The final
HEVC backend is added only after AVC420/AVC444 validation and must enter the same
codec-neutral registry; once its live interoperability gate passes, the production
selector gives it precedence over AVC and legacy codecs.

Architecture-specific optimization is limited to FFmpeg's existing x86/x86_64 assembly
and AArch64/NEON paths, plus measured local color/copy kernels. The protocol parser and
generation/frame state remain portable Rust.

## Fallback and failure behavior

- If EGFX is unavailable, keep the current Bitmap/RemoteFX session.
- If EGFX is available but no exact H.264 decoder is loaded, advertise a non-AVC EGFX
  capability only if the supported non-AVC EGFX codecs are implemented; otherwise do not
  opt into EGFX yet and retain legacy graphics.
- A decoder is not production-eligible until its exact wire profile has passed a live
  interoperability test. In particular, HEVC remains disabled during development and
  verification even if a local hardware capability probe or offline fixture succeeds.
- After HEVC is production-eligible and the server negotiates it, choose HEVC first. If
  HEVC negotiation or decode fails, fall back to AVC420/AVC444 according to their own
  gates, then RemoteFX and Bitmap. A failed HEVC attempt must not make the legacy path
  unavailable.
- If an advertised codec fails during decode, terminate that EGFX graphics generation
  safely and request the existing full-surface recovery path; never silently present stale
  or partially decoded pixels.
- Capability diagnostics contain only codec names, profile IDs, decoder backend IDs and
  state transitions; no credentials, raw H.264 payloads or server secrets.

## Validation gates

1. Unit tests for EGFX capability parsing, AVC capability filtering, PDU fragmentation,
   NAL length-prefix validation, decoder reset and generation-bound frame boundaries.
2. H.264 fixture tests for FFmpeg backend on every supported output format.
3. Cross-target compile/package tests for Windows i686/x86_64/arm64, Linux i686/x86_64/
   arm64 and macOS x86_64/arm64, with explicit missing-runtime behavior.
4. Live tests against a server that advertises EGFX AVC420 and a server that only exposes
   legacy Bitmap/RemoteFX. The latter must remain unchanged.
5. Performance measurements split into transport, EGFX parse, H.264 decode, pixel conversion,
   mailbox age and GPU present. No fixed latency claim is made without these measurements.
