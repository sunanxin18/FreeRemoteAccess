# 来源与许可证

本 crate 的 checked 接口、staging、SSE2 低 32 位乘法组合和 NEON/SSE2 操作适配层为新实现。lifting/band/prefix 算法按下列固定 IronRDP 数值契约改写为 SIMD；色彩公式按固定 yuv rdp.rs 改写。标量参考只存在于 dev 依赖或 tests。没有复制或链接 FreeRDP C 实现，不携带其 WinPR、decoder 或 dispatcher；FreeRDP 3.32.2 commit `97eff6ddfbbfaa8e42a126d362686bd001ff8bdc` 仅是既有研究候选。尤其没有采用其不同的 16-bit DWT/14-bit x86 色彩舍入。

- IronRDP graphics 0.9.0 registry archive SHA-256 `c7493e426b6a8104cd497e518ba7781a9c7fc9a5f58a9cc0c1111e6d66f63bcc`，upstream commit `11a0810cfbbabd8b8023875a05e3041216d4b01b`；MIT OR Apache-2.0，本 crate 选择 MIT，完整版权/许可见 `licenses/IronRDP-MIT.txt`。精确文件 hashes 见 `SOURCE-SHA256SUMS`。[源码](https://github.com/Devolutions/IronRDP/tree/11a0810cfbbabd8b8023875a05e3041216d4b01b/crates/ironrdp-graphics)
- yuv 0.8.17 registry archive SHA-256 `220655e1c245693beb13b377d3174b9efcb1645018d1d4ec53903fc211b135ae`，commit `64fd1eba280c0fc9371933b22dddf9e06630bdfb`；rdp.rs 的 BSD-3-Clause 完整头保存在 `licenses/yuv-BSD-3-Clause.txt`，保留 Radzivon Bartoshyk 版权。公式的实际 f32→i32 系数详见 README，不能替换为近似整数。[源码](https://github.com/awxkee/yuvutils-rs/blob/64fd1eba280c0fc9371933b22dddf9e06630bdfb/src/rdp.rs)
- 测试 wire 与图像/i16 fixtures 从同一 IronRDP commit 的 `crates/ironrdp-testsuite-core/tests/session/rfx.rs` 和 `tests/graphics/{rlgr,dwt}.rs` 常量逐值提取；只把 i16 序列化为 little-endian，将 u8 原样保存，无数值修改。原始文件 SHA-256 见 `SOURCE-SHA256SUMS`，生成 bytes 见 `tests/fixtures/SHA256SUMS`。完整 2970-byte wire 与 16384-byte期望图像均保留。tests 不依赖网络或 registry 源码绝对路径。
- 独立 session oracle `ironrdp-session =0.11.0` registry archive SHA-256 `3f1a5a20c24d8ebf47477f9f513201aeb894e7c7f779ea633ece65e42a9f0652`；只读 dev 依赖。PDU oracle `ironrdp-pdu =0.9.0` registry SHA-256 `4ccd1179a4d106df1930347701388b5c79bc3725fa7dab4438d57db0d9d19347`。

Cargo.lock 保留以上 registry pin；没有 `ironrdp-graphics` crates.io patch，也没有 vendored 第二份生产 decoder。未来任何 production dispatch 接入需另行审查并保留这些独立 oracle。

A2.1 packed Bitmap run（2026-10-13）：checked depth/length/mask契约、48byte周期pattern、有限mask准备与NEON/SSE2显式16bytecopy/XOR为本项目新实现；没有复制第三方parser、RLEdecoder或bulk libc实现。原始packed555/565/24语义遵循已冻结Task4A2.1接口合同；test-only逐pixeloracle独立于生产pattern/SIMD，555高位、555/565WHITE以及跨lane24bit交替预期值手写固定。tests/common仅为dev测试模块，不进入正常依赖。原A1来源/fixture/pin及许可证不变。
