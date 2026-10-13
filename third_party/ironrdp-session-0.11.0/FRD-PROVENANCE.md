# 固定生产副本

来源：crates.io `ironrdp-session-0.11.0`；原始 archive SHA-256 `3f1a5a20c24d8ebf47477f9f513201aeb894e7c7f779ea633ece65e42a9f0652`。
`UPSTREAM-SHA256SUMS` 保留复制时全部原始文件。许可证沿用 crate 内 LICENSE 文件。

A2.6 的精确 `FRD-MODIFICATIONS.json` 只允许 `src/rfx.rs`、`src/fast_path.rs`、`src/active_stage.rs`，每个 upstream/post SHA 固定；manifest、lock、PDU、image.rs 和 encoder 不改。

三个已构建对象的 `set_native_legacy_kernels` 显式保存同一个已检查 CPU 的 stateless 能力，默认 None。公有 ProcessorBuilder/ActiveStageBuilder 字段和默认行为不变；Stage 替换 FastPath Processor 时把已选 native policy 传给新 Processor，进而传给 RFX context。平台壳没有 ISA/codec 分支或生产选择。

RFX 三组件和 checked color 全部成功后才 apply 当前 tile，失败返回 custom SessionError with source；已经发布的前一 tile 保留。Bitmap 原生错误在 apply/Region 前返回 custom SessionError with source；默认 None 分支保留旧 warn+rectangle。实际 FastPath、slow path 共用 Processor。该路径仍使用旧 image scalar 像素输出，A2.7 前不代表 whole-codec/生产能力就绪。

来源全清单和独立原版 graph、完整 wire、image/partial publication、原版 panic/native checked error 区分与限制见 `tests/README.md`。没有 device/network/signing/GUI、协议广告或 live 互操作声明。

## A2.7B explicit complete native image consumers

Explicit selected capabilities now connect Bitmap/RDP6/RFX to the reviewed K row
API through graphics' neutral reexports. Default builders/None policy and legacy
image helpers preserve their previous semantics. Full source SW/SH, exact row
stride, bottom-up indexing by SH, logical upper-left crop, target rectangles and
256 MiB resource preflight belong to portable session adapters; K receives only
exact tight rows. Raw Bitmap includes every align4 row pad, including the last;
CODEC_ID_NONE retains the explicit baseline bottom-up tight BGR/BgrX convention.
SetSurfaceBits uses extended dimensions and ignores incoming right/bottom;
StreamSurfaceBits checks exclusive bounds and crops from the explicit source.
These source-layout choices are not new wire/live qualification evidence.

Native bitmap/tile methods return Option regions. No-op/outside/empty clips,
marker-only and no-tile RFX frames cannot manufacture an origin pixel update;
real (0,0) one-pixel updates still publish. Region32 preserves stock tile alpha
semantics, region order and 64-pixel source stride. All predictable row/geometry/
palette/capability checks precede pointer begin. A later rectangle/tile failure
retains earlier successful work and reports a typed source; this is not whole-PDU
rollback. Current K constructed capabilities have no remaining predictable row
errors after this preflight; a future unknown row/pointer failure after begin
can still retain prior rows/pointer side effects and is not promised atomic.

Native pending local mouse movement is retained across first header/Bitmap
failure, applied after a successful frame, or before a successfully decoded
pointer shape/hide/default event to preserve event order. A successful server
Position consumes/overrides pending local position. Cache misses and pointer
decode failures do not consume it; earlier successful events remain per-update
side effects. Cursor compositing and its legacy non-RGBA assumptions remain
unchanged and do not qualify all-format cursor SIMD/visual correctness.

`tests/fixtures/image-consumers-original.bin` contains 2,624 records frozen by a
separate registry-only original process after complete source/archive/graph
checks. Complete-source crop expectations copy pixels from stock-decoded full
images; there is no production-derived color oracle. The fixture includes all
64 Raw32 and Region32 source/target alpha combinations, multi-region tile output,
stock cursor bytes/background and public pointer-event order, and separate old
malformed behavior. Tests compile actual shipping sources and real public
Processor/Stage APIs; native call traces and mutations discriminate missing
completeness, geometry, alpha, native consumption and preflight publication.
The separate QOI helper test uses the reviewed independent stock 192-byte alpha
anchor. A2.7B round1 additionally compiles and runs the real QOI/QOIZ wire
branches in an ignored independent validation dependency workspace, including
non-test library compilation. Cargo verifies exact original-lock qoicoubeh 0.5.0,
zstd-safe 7.2.4 and zstd-sys 2.0.16+zstd.1.5.7 archives; root/oracle manifests,
locks and default feature graph stay unchanged. Native QOI validates the same
Decoder header's complete checked dimensions/channels/isize/256MiB allocation
budget, exact extended source W/H and crop shape before TryReserve/decode_to_buf.
Native QOIZ starts with at most 16KiB and gates every growth before TryReserve.
The limit is a local resource boundary, distinct from wire geometry or a display
resolution ceiling; 4K/8K geometry remains valid. Typed QOI/Kernel/TryReserve
sources survive; predictable failures do not publish pixels/cursor state or
consume pending movement. This preserves the existing per-update boundary.

14 optional tests plus 23 existing session tests run in debug/release, along with
strict scoped clippy, real default None old/new wire comparisons from fixed
848258c source, eight compiled/killed actual-source mutants per profile, and the
immutable 192-byte alpha anchor. Oversize allocation probes terminate before
System allocation; small-budget real zstd growth uses the same private helper
and cannot expand the 256MiB production gate. Dependency object identities and
commands are recorded in target/task-4a27b-repair and the round1 report. These
wire tests do not enable the optional codecs in the product cache graph and do
not qualify QOI algorithm SIMD, package/live/performance or production selection.
No production factory, advertisement, active-stage interface, UI, device,
package, protocol manifest/lock or server component is changed.
