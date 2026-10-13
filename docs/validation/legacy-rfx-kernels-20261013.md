# Legacy RemoteFX 数值 SIMD 验证

日期：2026-10-13。`frd-rdp-legacy-kernels` 是独立的 checked 数值后端，尚未接入生产 IronRDP graphics dispatch。本记录不将 Progressive、Bitmap/RDP6、legacy RemoteFX 或 OHOS 客户端整体标为已完成。

## 范围与契约

提供 NEON 与 SSE2 的 dequant、三层 inverse DWT、modulo16 differential、RLGR 零输出叶子和 i16 YCbCr→RGBA。架构选择在数值后端，正常依赖为空；原版 `ironrdp-graphics=0.9.0`、`yuv=0.8.17`、`ironrdp-session=0.11.0` 仅为开发期 oracle。没有改变原生产 wire/parser、状态机、广告或桌面调用路径，macOS 仍只支持 ARM64。

逐样本回归固定了两项不能直接移植的算术细节：DWT 高系数先在 i16 左移截断再扩展；yuv f32 常量转换得到 91915/46818/22526/115992，而不是候选 FreeRDP 的近似系数。色彩计算超出原表达式明确可表示的域时返回 `ColorOverflow`，不依赖 debug panic/release wrap 差异，也不修改输出。生产错误映射仍须后续集成审查。

## 实际执行

两台专用 OHOS ARM64 模拟器运行同一组最终 release 程序，各通过 parity 7、完整 wire 组合 4、guard-page 1，共 12 项。保护页测试将有效 slice 紧贴不可访问页，以捕获尾部越界读写；完整 wire 测试保留原版 session decoder 和预期图像作为独立比较。软件解码组合仍使用原版 RLGR parser 及其 materialization；新 zero 叶子仅独立测试，没有冒充生产接线。

最终测试产物 SHA-256：

| 程序 | SHA-256 |
|---|---|
| parity | `5eaf9214b654425f1181faf22cd14874d5336454094863274bf71836aa955f23` |
| wire composition | `6144e90d79cc3d5f647dd34990d00b8ce7e050de83e657fca37c98ebb8160143` |
| guard pages | `25e9b7100f200fd22bea0da0da3b24578b2474828223d2a225724615a75a8b27` |
| benchmark | `dda2fb37b528dbc1e9fad6bf7d081d4f38c45cf76c428f6f87c3538d2710660e` |

## 性能结果与限制

停止宿主构建后，按手机/PC 交替顺序各执行三轮；每轮 128 warmup、501 samples、每样本 16 次。下表为完整三分量 tile 测试组合的 median，单位 ns；全部原始 p95 和各叶子结果在本地 `target/harmony-validation/task4a1/final-*-benchmark-r*.log`。输入 SHA-256 为 `874fb96a67c766e36f3279c48e9cceecc84e02584be804e5f30c0e2fe5d9d980`。

| guest | native 三轮 median | 原版 oracle 三轮 median |
|---|---|---|
| 手机 | 80099 / 80054 / 79901 | 73489 / 73414 / 73588 |
| PC | 80138 / 80161 / 80395 | 73604 / 73848 / 73643 |

本轮 native 约慢 8.5–9.1%，**没有证明提速**。最初与宿主构建可能重叠的一次 PC 测试曾显示相反方向，随后三轮不再复现，不能选择该次结果宣称改善。原版 oracle 本身也被 LLVM 向量化；native 色彩路径还包含完整域预检，两者并非“SIMD 对纯标量”的比较。模拟器数值基准不是实际 RDP 帧率或交互延迟。

`c3ae798` 独立审查通过，无 Critical/Important；新 crate clippy 干净，相邻 RDP 测试为 321 passed / 3 ignored，伴随四项既有 dead_code warning。仍需完成 Bitmap/RDP6 内核、生产 dispatch 与错误映射、完整 RLGR materialization 接入，以及每个 Windows/Linux 架构的实际执行、包和真实会话验收。现有桌面生产路径保持不变；不能用这一独立 crate 开放鸿蒙连接或扩展 codec 广告。
