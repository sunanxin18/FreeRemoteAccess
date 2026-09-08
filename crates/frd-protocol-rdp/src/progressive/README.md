# RemoteFX Progressive 解码边界

本模块按 MS-RDPEGFX 2.2.4.2 / 3.3.8.2 将容器、熵位流、持久状态和数值 SIMD 分开。
实现针对 Windows/Linux i686、x86_64、ARM64 及 macOS ARM64；不支持 macOS Intel。

- `wire.rs` 精确限定 blockLen、tileDataSize、嵌套 tile、输入与数量预算。
- `entropy.rs` 严格解析 RLGR1、RAW/SRL，跨 subband 保持 reader/KP，拒绝截断补零。
- `state.rs` 按 surface/tile 保留 DecDwtQ reference，按 codec context 保存 DAS/量化状态；
  DeleteContext 不删除 surface reference，DeleteSurface/reset 释放。EGFX frame coverage 与
  codec 内部 frame 分离，payload 失败不提交 staged state。
- `kernels.rs` 使用 SSE2/NEON 完成 differential、量化、sign、refinement、两种逆 DWT 和
  11.5 定点 BGRA；无 native CPU backend 时不可生产构造。标量 oracle 仅在测试中。
- `native.rs` 接通严格熵解码、surface reference、DAS 和 SIMD；保留系数域，使用副本渲染。

base quant 为 0..15，progressive quant 为 0..8。有效左移为 base+progressive-1，
本实现明确拒绝不可表示的负 shift；合法非负 shift 上限22。移位在32位进行再存低16位。
CONTEXT flag 0x1 是 subband diffing，REGION flag 0x1 才是 reduce-extrapolate。

## 参考与许可

依据微软公开规范独立实现 Rust 状态及位流解析。数值算法参考 FreeRDP
`54a873e2710710841c6ec2b756df64285ec6e29a`，保留 Apache-2.0
[许可文本](LICENSE-APACHE) 及源码中的归属说明。FreeRDP 没有作为运行时库引入。

| 参考文件 | SHA-256 |
| --- | --- |
| libfreerdp/codec/progressive.c | 102858436412f55a3042dfaefc1ce4d2135cc3fa8be0a58e51ed412f51201605 |
| libfreerdp/codec/rfx_dwt.c | 6a72283bede3569f58d02e72b063e712eb0270d5cfecab4952f392bed17fa80b |
| libfreerdp/primitives/prim_colors.c | f9156ebc5a0598b637b625a937b0e84f8d10ffc8ad80839a344354d669add221 |

归属：Marc-Andre Moreau (2014)、Armin Novak / Thincast Technologies GmbH (2019)、
Vic Lee、Stephen Erisman、Norbert Federa、Martin Fleisz (2011)、
Hewlett-Packard Development Company, L.P. (2012)。

## 验证边界（2026-09-08）

本机 ARM64 41 项组合测试通过，1 项手动 benchmark 默认忽略。涵盖 wire、熵位流、
状态事务、真实 RLGR 编码器产生的4096系数流、差分 reference、新context、RAW跨band升级。
独立编译固定 FreeRDP C 数学参考：普通/Reduce-Extrapolate DWT 各100组，颜色102组，
逐系数/像素相同；8个 DWT 输入输出指纹已固化到kernel tests。

SSE2/NEON六个Windows/Linux target的kernel编译不等于目标运行；完整七目标runtime、
完整客户端 GUI 与恢复验收仍未完成；后续有界混合流证据见下文。不能将这些测试作为生产EGFX开放依据。

EGFX接线回归：285项RDP测试通过（3 ignored），实际PDU覆盖native first/upgrade、
offscreen映射、DeleteContext保留reference、1080p 510tile精确合并与失败清理。
上述测试为接线阶段证据。后续 `707b178` 授权探针已取得持续 20 秒/70 帧的
真实混合流记录，但 AVC 计数为 0，不能据此验收 AVC420/AVC444。当前源码核对
至 `50bb103`：`WireToSurface2` 已接通实验性 Progressive 路径；默认生产广告仍为
`LegacyOnly`。最新修订的完整七目标 parity/benchmark、客户端 GUI 与恢复验收
仍开放，见 [验证记录](../../../../docs/validation/rdp-egfx-h264-20260907.md)。
