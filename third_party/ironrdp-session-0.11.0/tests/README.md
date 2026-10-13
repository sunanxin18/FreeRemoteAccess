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

## A2.7B round1 可选 QOI/QOIZ 分配前验证

产品 graph 的 feature/default、root/oracle manifest 和 lock 不变。额外依赖仅放在 ignored
`target/task-4a27b-repair/optional-deps` 独立 workspace；`--prepare-deps` 使用正常 Cargo
解析和 checksum fetch，按本包原 `Cargo.lock` 固定每个共享 registry package，生成自己的锁。
随后 build 与 harness 全部 `--offline --locked`。依赖是 `qoicoubeh 0.5.0`（真实 lib 名为
`qoi`）、`zstd-safe 7.2.4`、`zstd-sys 2.0.16+zstd.1.5.7`，无 parser shim。

```sh
export PATH=/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin:$PATH
python3 third_party/ironrdp-session-0.11.0/tests/run_image_optional_tests.py --prepare-deps --clippy --verify-old-default
python3 third_party/ironrdp-session-0.11.0/tests/run_image_optional_tests.py --release --clippy --verify-old-default
python3 third_party/ironrdp-session-0.11.0/tests/run_image_optional_tests.py --production --clippy
python3 third_party/ironrdp-session-0.11.0/tests/run_image_optional_tests.py --production --release --clippy
python3 third_party/ironrdp-session-0.11.0/tests/test_image_optional_mutations.py
python3 third_party/ironrdp-session-0.11.0/tests/test_image_optional_mutations.py --release
```

harness 编译实际 shipping session 全部相关 modules，并启用真实 `qoi+qoiz` 分支；
`--production` 另编译不含 `cfg(test)` 的 library。14 项 optional tests 加既有 23 项
session tests，涵盖 64 次 RGB/RGBA×两 codec×八 target format×Set/Stream 对照、64 次
native Stream crop、16 次原版冻结 alpha anchor、typed allocation/header/decode 错误、
初始/增长 staging gate，以及像素/可见鼠标背景/pending move 的失败无发布边界。
`--verify-old-default` 另从固定 `848258c2a9b90f71144f9b5c5f43694a748e614c` Git 对象
取只读 source snapshot，运行相同 64 个公开 None wire cases，比较完整 image/state/updates
结果 digest；不改变 checkout。老默认分支仍保留原行为。

QOI 使用同一个 Decoder::new/header 后 checked 完整 W×H×channels、isize/256MiB 资源门、
精确 ext W/H 和 crop 几何，再 TryReserve/decode_to_buf；不重 parse 或依赖事后 vec 长度。
QOIZ 首次 staging ≤16KiB，所有增长先过同一资源门，再 TryReserve。超限 fixture 和
mutants 的 test-only allocator 在触大内存前退出73；增长使用同一 shipping helper 的
1024-byte 更小 budget，不能放大生产 256MiB 限额。外层 fragmentation 仍可复制输入；
初次 staging probe 直接调用实际 Surface dispatch 以区分这两次分配。

两 profile 各8个实际 shipping 源码 mutants 必须成功编译再失败，覆盖提前无限 vec、
异形源布局、压缩长度初始 reserve、增长门绕过、preflight 后移、TryReserve source 丢失
和回到 scalar helper。原2,624-record fixture 未改；可选线码正确性/分配前资源门证据
不代表 QOI 算法 SIMD、产品启用、跨架构包、性能或 live 生产合格。

## A2.8B 精确 session 内容覆盖

`image::ActiveStageProcessResult` 的 `outputs` 保持原枚举；每个 GraphicsUpdate
对应一份带 `graphics_output_index` 的 receipt，包括 cursor/sentinel 的明确空内容。
新 rich Stage API 共用一次实际 parser/dispatch，旧公开入口禁用 collector。
默认 rich 路径从实际 RLE written 或 RDP6 完整形状事实验证成功，再按完整 SW/SH、
raw padding 与逻辑左上裁剪复用原转换 helper；不按预分配长度推断完整性。
原生路径仍须显式选择受检查能力，工厂/协议广告不变。

```sh
export PATH=/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin:$PATH
export PYTHONDONTWRITEBYTECODE=1
python3 third_party/ironrdp-session-0.11.0/tests/run_decoded_coverage_tests.py --clippy
python3 third_party/ironrdp-session-0.11.0/tests/run_decoded_coverage_tests.py --release --clippy
python3 third_party/ironrdp-session-0.11.0/tests/run_decoded_coverage_tests.py --optional --clippy
python3 third_party/ironrdp-session-0.11.0/tests/run_decoded_coverage_tests.py --optional --release --clippy
python3 third_party/ironrdp-session-0.11.0/tests/run_decoded_coverage_tests.py --production --clippy
python3 third_party/ironrdp-session-0.11.0/tests/run_decoded_coverage_tests.py --production --release --clippy
python3 third_party/ironrdp-session-0.11.0/tests/run_decoded_coverage_tests.py --production --optional --clippy
python3 third_party/ironrdp-session-0.11.0/tests/run_decoded_coverage_tests.py --production --optional --release --clippy
python3 third_party/ironrdp-session-0.11.0/tests/test_decoded_coverage_mutations.py
python3 third_party/ironrdp-session-0.11.0/tests/test_decoded_coverage_mutations.py --release
python3 third_party/ironrdp-session-0.11.0/tests/run_image_consumers_tests.py --clippy
python3 tests/legacy-oracle/verify_sources.py
python3 tests/legacy-oracle/verify_catalog.py
```

`--optional` 只复用已经准备好的独立可选依赖 workspace，仍 offline/locked，
不修改产品 features、manifest 或锁。旧14项 QOI/QOIZ 测试继续包含；新的默认/原生
QOI/QOIZ crop 测试覆盖 RGB/RGBA 和八种目标格式。非可选 harness 的分配失败探针
只存在于新 runner 的私有壳，可选壳复用原 allocator；shipping allocator 不变。
既有 image-only QOI runner 仅补真实 Stage output/fast_path reexport。

新增测试包含真实 FastPath/X224 两端点留空和后续中点、完整 raw/packed/RDP6
裁剪、奇数 chroma/alpha、最终 padding、原版632份适用冻结图像记录、typed underfill
与 decoder-error 空内容、软件 cursor 和 palette/control 输出索引、分片完成后同一次
Stage 调用的65,536/65,537限额、try_reserve 失败先于 image/cursor begin，以及
独立小网格 RFX 实际像素和 coverage 成员枚举。RFX 采用 checked、排序且允许重叠
的 clip/交集列表，rich writer 与 receipt 共用同一列表，避免上游 inclusive Region
union/simplify 丢单行或缩短不等高 band；旧入口保留原行为。

元数据 raw rectangles 上限512KiB；receipt/index/Vec header 另限65,536项，64位ABI
最多2MiB、32位最多1MiB。另有≤512KiB RFX clip 列表和≤512KiB单 tile精确交集，
decoder、staging、输入重组、原 outputs、allocator 开销和 framebuffer 分别计费。
预算不能每次 update 重置，也不能 bbox 回退。Err 不返回部分 receipts；之前成功
像素或鼠标动作可能保留，调用者必须终止/重置，不提供整 PDU 回滚。

所有九个 mutation 必须实际编译成功后被像素/元数据语义拒绝。原2,624-record
fixture、原版 archive/source inventory、generator 和 allowlist 不变；最终 hash
记录在 FRD-MODIFICATIONS.json 与本次报告。host 测试不是 OHOS phone/PC、跨架构、
HAP、live、软件 codec SIMD/性能或生产选择的资格证据。
