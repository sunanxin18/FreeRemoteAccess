# 固定生产副本

来源：crates.io `ironrdp-session-0.11.0`；原始 archive SHA-256 `3f1a5a20c24d8ebf47477f9f513201aeb894e7c7f779ea633ece65e42a9f0652`。
`UPSTREAM-SHA256SUMS` 保留复制时全部原始文件。许可证沿用 crate 内 LICENSE 文件。

A2.6 的精确 `FRD-MODIFICATIONS.json` 只允许 `src/rfx.rs`、`src/fast_path.rs`、`src/active_stage.rs`，每个 upstream/post SHA 固定；manifest、lock、PDU、image.rs 和 encoder 不改。

三个已构建对象的 `set_native_legacy_kernels` 显式保存同一个已检查 CPU 的 stateless 能力，默认 None。公有 ProcessorBuilder/ActiveStageBuilder 字段和默认行为不变；Stage 替换 FastPath Processor 时把已选 native policy 传给新 Processor，进而传给 RFX context。平台壳没有 ISA/codec 分支或生产选择。

RFX 三组件和 checked color 全部成功后才 apply 当前 tile，失败返回 custom SessionError with source；已经发布的前一 tile 保留。Bitmap 原生错误在 apply/Region 前返回 custom SessionError with source；默认 None 分支保留旧 warn+rectangle。实际 FastPath、slow path 共用 Processor。该路径仍使用旧 image scalar 像素输出，A2.7 前不代表 whole-codec/生产能力就绪。

来源全清单和独立原版 graph、完整 wire、image/partial publication、原版 panic/native checked error 区分与限制见 `tests/README.md`。没有 device/network/signing/GUI、协议广告或 live 互操作声明。
