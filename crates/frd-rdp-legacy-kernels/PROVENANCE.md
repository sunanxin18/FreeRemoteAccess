# 来源与许可证

本 crate 的 checked 接口、staging、SSE2 低 32 位乘法组合和 NEON/SSE2 操作适配层为新实现。lifting/band/prefix 算法按下列固定 IronRDP 数值契约改写为 SIMD；色彩公式按固定 yuv rdp.rs 改写。标量参考只存在于独立原版 oracle workspace 或 tests。没有复制或链接 FreeRDP C 实现，不携带其 WinPR、decoder 或 dispatcher；FreeRDP 3.32.2 commit `97eff6ddfbbfaa8e42a126d362686bd001ff8bdc` 仅是既有研究候选。尤其没有采用其不同的 16-bit DWT/14-bit x86 色彩舍入。

- IronRDP graphics 0.9.0 registry archive SHA-256 `c7493e426b6a8104cd497e518ba7781a9c7fc9a5f58a9cc0c1111e6d66f63bcc`，upstream commit `11a0810cfbbabd8b8023875a05e3041216d4b01b`；MIT OR Apache-2.0，本 crate 选择 MIT，完整版权/许可见 `licenses/IronRDP-MIT.txt`。精确文件 hashes 见 `SOURCE-SHA256SUMS`。[源码](https://github.com/Devolutions/IronRDP/tree/11a0810cfbbabd8b8023875a05e3041216d4b01b/crates/ironrdp-graphics)
- yuv 0.8.17 registry archive SHA-256 `220655e1c245693beb13b377d3174b9efcb1645018d1d4ec53903fc211b135ae`，commit `64fd1eba280c0fc9371933b22dddf9e06630bdfb`；rdp.rs 的 BSD-3-Clause 完整头保存在 `licenses/yuv-BSD-3-Clause.txt`，保留 Radzivon Bartoshyk 版权。公式的实际 f32→i32 系数详见 README，不能替换为近似整数。[源码](https://github.com/awxkee/yuvutils-rs/blob/64fd1eba280c0fc9371933b22dddf9e06630bdfb/src/rdp.rs)
- 测试 wire 与图像/i16 fixtures 从同一 IronRDP commit 的 `crates/ironrdp-testsuite-core/tests/session/rfx.rs` 和 `tests/graphics/{rlgr,dwt}.rs` 常量逐值提取；只把 i16 序列化为 little-endian，将 u8 原样保存，无数值修改。原始文件 SHA-256 见 `SOURCE-SHA256SUMS`，生成 bytes 见 `tests/fixtures/SHA256SUMS`。完整 2970-byte wire 与 16384-byte期望图像均保留。tests 不依赖网络或 registry 源码绝对路径。
- 独立 session oracle `ironrdp-session =0.11.0` registry archive SHA-256 `3f1a5a20c24d8ebf47477f9f513201aeb894e7c7f779ea633ece65e42a9f0652`；位于独立 oracle workspace。PDU oracle `ironrdp-pdu =0.9.0` registry SHA-256 `4ccd1179a4d106df1930347701388b5c79bc3725fa7dab4438d57db0d9d19347`。

`tests/legacy-oracle/Cargo.lock` 保留以上 registry pins。根 workspace patch 使用 graphics/session 固定生产副本；oracle 自有 `[workspace]` 和锁且根 exclude，不继承 patch。生产副本 source 尚未改变，私有 graphics→kernel 依赖没有新增 decoder 调用。

A2.1 packed Bitmap run（2026-10-13）：checked depth/length/mask契约、48byte周期pattern、有限mask准备与NEON/SSE2显式16bytecopy/XOR为本项目新实现；没有复制第三方parser、RLEdecoder或bulk libc实现。原始packed555/565/24语义遵循已冻结Task4A2.1接口合同；test-only逐pixeloracle独立于生产pattern/SIMD，555高位、555/565WHITE以及跨lane24bit交替预期值手写固定。tests/common仅为dev测试模块，不进入正常依赖。原A1来源/fixture/pin及许可证不变。

A2.2 RDP6 planar（2026-10-13）：新的 checked layout/API、NEON/SSE2 delta/color/chroma/pack 和有限 staging 按固定原版 `rdp6/rle.rs` 与 `rdp6/bitmap_stream/decoder.rs` 语义实现；原文件及 PDU layout 文件精确 hashes 已追加到 `SOURCE-SHA256SUMS`。没有复制 parser 或 dispatcher，没有新增正常依赖或修改 registry。

独立固定 fixture `tests/fixtures/planar-original.bin` 的 expected bytes 由原 `examples/generate_planar_fixture.rs`（现迁到 `tests/legacy-oracle/src/planar.rs`）直接调用未修改 registry `ironrdp-graphics =0.9.0` 的公开 `rdp6::BitmapStreamDecoder` 输出，生成器不调用 NativeKernels 或 test-only 色彩公式。原版消费输入含 alpha plane、raw padding 和 RLE delta；64 条记录保留原始 wire 与原版 output。生成源码 hash、archive/source hashes、fixture hash、binary schema、精确 command 与判别范围见 `tests/fixtures/planar-original-provenance.json`，fixture hash 同时保存在 `tests/fixtures/SHA256SUMS`。这些 bytes 在 native 实现前生成。普通 test-only scalar model 另作穷举与尺寸组合 oracle，不替代原版固定输出。当前根无 graphics/session patch；未来 A2.3 必须先隔离原版 oracle 再增加 production patch，不能让 dev oracle 自动指向新实现。


A2.3 oracle 拓扑隔离（2026-10-13）：原 A1 对原版函数的每个输入冻结为 `original-leaves.bin.gz`，共25962唯一输入键，key 为 operation + 每段 LEu64 长度 + 原始 LEi16/quant bytes 的 SHA-256。输入相同的重复调用共用一条记录；未抽样/删减4096脉冲或极值。原版输出是 real registry API 运算，不是复制数值公式。完整二进制结构与再生成见独立 runner README，压缩 SHA-256 sidecar 在 native tests 中验证。`original-composition.bin` 保存当前完整wire真实解析的三组quant/RLGR系数/decoded planes，kernel只消费固定捕获结果。原版64条planar数据及全部旧fixture未变。

四份原版 crate 的 archive 与全部 source hashes 见 `tests/legacy-oracle/SOURCE-INVENTORY.json`；vendored graphics/session 的原始清单见各自 `UPSTREAM-SHA256SUMS`。新增 standalone 完整 wire catalog、原版错误/profile测试、同边界基准与重新生成验证均不依赖 kernel。根 patch 前后已审查 Cargo metadata 图；dependency alias 不视为隔离手段。当前证据只覆盖 macOS ARM64 host 数据生成与测试，不增加其他目标 runtime/live 声明。

自审补齐：`original-rlgr-replay.bin.gz` 冻结独立原版catalog所有546个成功RLGR输出，保留旧逐byte截断/modes/terminal padding的native零区间重放覆盖；每条保留catalog索引及exactbytes，SHA256SUMS/sidecar/独立verify均覆盖，不成为parser接线证据。
