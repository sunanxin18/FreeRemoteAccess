# 固定生产副本

来源：crates.io `ironrdp-graphics-0.9.0`；原始 archive SHA-256 `c7493e426b6a8104cd497e518ba7781a9c7fc9a5f58a9cc0c1111e6d66f63bcc`。
`UPSTREAM-SHA256SUMS` 永久记录复制时全部原始文件，不随私有接线重写。
许可证沿用 crate 内 LICENSE 文件。

A2.3 仅适配 Cargo.toml 的本地 kernel 依赖；A2.4 在 `src/rle.rs` 增加显式 checked Bitmap native 入口、
共享 parser 的 materializer、native leaf dispatch 与测试导入。原 `decompress` 及各 bpp entrypoints 继续走 scalar materializer，
保留原命令级 first-line/insert-FG、FG 无条件源长度检查、empty/incomplete 接受、nonempty dst 和错误顺序。
没有 session dispatch、PDU、RDP6、媒体、平台壳或 kernel 数值变更。

`FRD-MODIFICATIONS.json` 给出这两个原文件的 upstream/post SHA-256 与新增测试/fixture identities。
`tests/legacy-oracle/verify_catalog.py` 仅接受这两个明确原文件的精确 posthash；其他原文件仍必须与原清单匹配，
不能向记录追加任意 source path 扩大豁免。原 registry source/archive 验证不受此例外影响。
新增 wire fixtures 只来自冻结 A2.3 catalog 或 hash 固定的原版 registry rle.rs，生成/测试方法见 `tests/README.md`。

A2.5 另在 `src/rdp6/rle.rs` 与 `src/rdp6/bitmap_stream/decoder.rs` 接入显式 checked native RDP6入口。
wire/plane controller由旧scalar和native共享；copy/fill/delta与RGB/YCoCg叶子直接用于实际解析，
所有RLE alpha仍解码消费，CLL0+CS保持ARGB，全部平面成功才append RGB。Kernel失败没有scalar重试。
旧入口/default及encoder/PDU/session不变。native另做零尺寸、产品/isize/append算术预检与reserve失败映射，
RGB/append的unsafe geometry在PDU之前拒绝，header-specific alpha/chroma总长在PDU之后但写入之前拒绝；原版的零尺寸接受/报错、overflow panic不属于native兼容承诺。
私有scratch可在parser失败后变化，已存在dst前缀保持且失败不追加RGB。

修改清单现在精确允许manifest、Bitmap source、这两个RDP6 source共四个posthash；其他原文件仍严格核验。
新增fixture只提取冻结原catalog，或通过hash-pinned原版actual source process产生，未用patched实现生成期望值。
六组bin/BMP与许可证沿用原 `UPSTREAM-SHA256SUMS`/隔离oracle source inventory，未复制修改或重写原始清单。
public bitmap无bytes-consumed字段；测量只来自private plane原版成功返回值。

## A2.6 显式 RLGR/RFX bridge

仅 `src/rlgr.rs` shared zero materializer、`src/lib.rs` module 声明和新增 `src/legacy_native.rs`。原版 encoder 保留，RLGR default 保留原标量 zero；显式 native 在真正 run 和终端 pad 两处调用已有能力 zero 叶子。component 检查两个 4096 长度，映射明确十个 Quant 字段，依次 RLGR→LL3[4032..4096] differential→dequant→DWT，并返回有 source 的 typed error。没有 ISA 分支或重新选择 CPU，没有 K/数值/Cargo 修改。

独立原版 RLGR/session/component fixtures 及 generator 来源、hash、命令和 checked/default 差别记录于 session vendor `tests/README.md`。`FRD-MODIFICATIONS.json` 扩展的两个原始路径均固定 upstream/post SHA；A2.3/A2.4/A2.5 四个既有记录不变。新消费者没有生产启用，image.rs/A2.7 仍是剩余 whole-codec 门。

## A2.7B explicit complete Bitmap consumer

The same shared RLE parser now reports actual `written` and `consumed` counters.
Existing scalar and native parity wrappers discard those counters and retain their
previous behavior; `decompress_native_complete` requires exact full-source output
before the private buffer can be applied. Its separate typed error retains the
native decoder or allocation error as a source and names incomplete progress.
It validates checked full source sizes and enforces the named 256 MiB private
Bitmap resource limit before reserve/resize. This resource limit is independent
of destination cropping and does not impose a 2560×1440 resolution limit; 4K/8K
geometry remains representable. Defaults and previous compatibility entrypoints
do not acquire the strict completeness/resource gate.

The architecture-neutral bridge reexports only the already reviewed closed
pixel source/format/depth types. Session consumes the RLE resource constant from
the portable graphics RLE module. There is no new ISA
selection, dependency or scalar production fallback. Focused original image
fixtures, public session tests and mutations are owned by the session vendor;
A2.4 Bitmap parity remains an independent unchanged-behavior regression gate.

## A2.8A portable scalar completion facts

`rle::decompress_observed` 返回固定大小 `RleDecodeProgress { written, consumed }`，
权威来自同一个 scalar/native wire parser 的真实 destination/source cursor。
原 scalar helper 同序执行 bpp 分派、零尺寸判断、原算术、resize 和单次 scalar materialization，
旧 public wrappers 丢弃事实。成功欠填充仍成功；Err 保留原错误顺序和 private prefix，
不增加资源门、错误类型、二次解析、strict/native fallback 或 CPU 选择。

`BitmapStreamDecoder::decode_bitmap_stream_to_rgb24_observed` 调用旧 scalar 入口一次，
只有原平面和完整 RGB writer 都成功之后，才对正且 checked 的源尺寸返回 `(SW, SH)`。
四字节/像素的 checked 上界同时覆盖 RGB24、完整 alpha+color planes 和奇数 chroma。
失败时保留旧错误和 private scratch；成功零尺寸/算术不可证明只返回 `None`。
不读追加 Vec 长度作为证明，不要求消费尾部，不修改 plane parser 或 session。

新增测试与 runner 身份记录于 `FRD-MODIFICATIONS.json`。原版 archive、SOURCE-INVENTORY、
UPSTREAM-SHA256SUMS、原 fixtures/generators、修改原件 allowlist 均保持原身份。
runner 使用既有 Cargo JSON/source harness、固定 Rust 1.96.0 和独立 registry-only oracle。
观察器窄测试覆盖实际 production library、真实 cursor、空源/欠填充/全零合法输出、
8/15/16/24bpp、错误/private prefix、ARGB/AYCoCg/alpha/奇数 subsampling、append/tail、
原版完整和畸形记录，以及无完成计数/resize 伪计数故障变体。

本门仅给出当次 portable 解码事实，不认证 destination crop/padding/stride、
精确 session receipts、baseline、生产软件热路径、跨目标运行或 live 互操作。
