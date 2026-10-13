# A2.6 实际 RLGR/RFX/session 消费者验证

仅显式 fixture 选择 `NativeKernels`，工厂、协议广告与构建器默认不启用 native。
`run_rfx_tests.py` 使用 Rust 1.96、Cargo offline/locked JSON 中的确切 rlib，并直接编译实际 graphics RLGR/bridge 与 session 各模块；未复制 parser。上游 graphics dev 依赖 bmp/expect-test 未缓存，故这不是完整上游 test suite。

```sh
export PATH=/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin:$PATH
python3 third_party/ironrdp-session-0.11.0/tests/run_rfx_tests.py
python3 third_party/ironrdp-session-0.11.0/tests/run_rfx_tests.py --release
python3 third_party/ironrdp-session-0.11.0/tests/run_rfx_tests.py --clippy
python3 third_party/ironrdp-session-0.11.0/tests/test_rfx_mutations.py
python3 third_party/ironrdp-session-0.11.0/tests/freeze_rfx.py --verify \
  --original-graphics /path/to/registry/ironrdp-graphics-0.9.0 \
  --original-session /path/to/registry/ironrdp-session-0.11.0
```

独立原版图和四份 archive/source 全清单验证后，generator 链接独立 oracle workspace 的原版 graphics/session/PDU。参数指定的原源码目录逐文件匹配来源清单。所有 expected 来自该独立 process 的原版 API；没有 K 或 production patch。新增 LL3、leading run、完整两 tile wire、channel 错误与 color overflow 样本用原版 encode/parser 生成。

| frozen 文件 | records | SHA-256 |
| --- | ---: | --- |
| rfx-original-catalog.bin | 634 RLGR + 10 session | 9c03b62a6d53dcfe01ea4add02371f7d7aa338299330e4889f406f5e99d540b3 |
| rfx-original-supplemental.bin | 837 RLGR + 24 component + 18 session | 9fbccd33f4d3de61ea170805077d6b41f66f2dd305adb0b15d1851956aed1bfe |
| rfx-original-quant-order.bin | 6 component | 635252cf344eb283f9328296c8602d37ef2dc68a2bfbbd4ea29f3b7f922bcbf2 |

最初 quant `[0,1,...9]` 的 HL1/LH1 均不左移，因此 HL1/LH1 交换 mutant 不可区分。独立原版追加第三份 fixture，使用 `[2,3,...11]` 与两个 band 的非零系数；旧两份 fixture 没有改变。

格式 `FRDRFX01` + LE u32 record count；每 record 是六个 LE u32 长度前缀字段：name、meta、wire、initial、expected/partial、result。meta 1/3 为 RLGR1/3 + LE u32 output size；11/13 为 component RLGR1/3 + 十个 HL1,LH1,HH1,HL2,LH2,HH2,HL3,LH3,HH3,LL3 量化值；0 为 64×64 session，20 为 128×64 session。i16 以 LE 编码，尺寸在读取时只从 u32 转 usize，适用 i686；没有截断 u64 几何。

真实 2970-byte wire 的 image 是 16384-byte 原版 captured fixture。另有原版真实裁剪、两个 tile、第一/第二 tile 各 channel EmptyTile 及 checked ColorOverflow。失败 tile 不发布，先前成功 tile 保留；没有整帧回滚保证。原版 q17 component 与域外 color 的 debug panic 单列，native 分别返回 InvalidQuant/ColorOverflow，不伪称完全 parity。

RLGR test-only trace 位于实际 parser 零调用点；component trace 在实际 RLGR/differential/dequant/DWT 返回点，记录顺序和 64 个 LL3。mutation runner 只写 target 下独立源码副本，要求每个 mutant 编译成功并被语义/trace 测试拒绝。零 run/pad bypass、LL3 错范围、量化互换、再激活 policy 丢失、RFX capability 丢失全部覆盖。

Scoped clippy 严格拒绝新 warnings，仅命名豁免两个上游既有问题：RLGR encoder 的 `missing_panics_doc` expectation 在该窄 harness 中无对应 warning (`unfulfilled_lint_expectations`)，以及原版 FastPath::process 的 pointer move 嵌套 if (`clippy::collapsible_if`)。仅 scoped RLGR harness 对未消费的原版 Bits utility 标记 dead_code；未改变 shipping utility 源码。

A2.7 边界：image.rs 仍走原版 scalar row/pixel conversion/cursor；新 policy 是中间显式消费者路径。当前 Bitmap parser 在 EOF 可能成功并保留预分配未覆盖部分，declared width/height 和目标 rectangle 的几何关系亦未在本任务规范化。成功测试均为完整覆盖且几何一致；Region 本身不能证明全幅 successful baseline。目标 OHOS phone/PC、跨架构运行、打包、live 与生产选择由 root 后续验证。
