# Legacy RemoteFX 数值 SIMD 验证

日期：2026-10-13。`frd-rdp-legacy-kernels` 是独立的 checked 数值后端，已有显式 Bitmap/RDP6 消费者，session 生产默认尚未切换。本记录不将 Progressive、Bitmap/RDP6、legacy RemoteFX 或 OHOS 客户端整体标为已完成。

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


## Bitmap packed 叶子增量（A2.1）

`563de87` 在同一能力对象下新增 `PackedDepth` 与 copy、fill、XOR previous、LSB mask、双像素 dither 五类 checked API，支持 Index8、RGB555、RGB565、BGR24。独立审查通过，无 Critical/Important/Minor。此阶段不改变任何 parser、生产 dispatcher 或 codec 广告。

全量块以显式 NEON 或 SSE2 搬运/异或；最多 48-byte 周期布局和 24-byte mask 准备是有界控制处理，所有非空数值短尾通过本地 16-byte staging 执行相同 SIMD。packed24 的三字节/六字节相位跨 lane 保持，555 原始高位保留，首行与跨行依赖由未来 parser 调用者决定。长度、pixel 尺寸和 mask 范围在写输出前全部验证，错误不改变目标。

macOS ARM64 debug/release 各通过 19 项有效测试（包含既有 12 项 A1），另有两个显式 benchmark 默认 ignored；覆盖全部 256 mask、全部合法 offset/count、未对齐地址、奇数 dither split、非法输入原子性与保护页短尾。包级 clippy `-D warnings`、fmt 和 diff 检查通过。实际优化产物的完整块与短尾包含向量指令。

Windows/Linux 各 i686、x86_64、ARM64，macOS ARM64 与 OHOS ARM64 共八个目标完成独立优化对象编译及反汇编。A2.1 冻结时，除 macOS ARM64 外尚未执行目标正确性或基准；后续 OHOS 结果见下节。对象生成不证明运行、链接、应用包或会话能力，未构建 macOS Intel。

宿主基准使用 128 次预热、501 个样本，每样本 64 次、预分配输出。对照是普通优化 Rust 循环或 slice copy，可能由编译器向量化，且不包含 native checked 入口的同等预检成本。下表是 BGR24、1024 pixels 的 median/p95，单位 ns：

| 叶子 | native | 对照 |
| --- | --- | --- |
| copy | 80 / 82 | 29 / 31 |
| fill | 106 / 108 | 1826 / 1876 |
| XOR | 127 / 131 | 2091 / 2132 |
| dither | 110 / 111 | 1826 / 1878 |

较大块 fill/XOR/dither 对本对照有改善，但 copy、小块及部分 mask 较慢；没有据此宣称完整 Bitmap 更快。单次宿主测量没有 CPU 绑定或频率隔离，原始测试、对象、反汇编和 120 组测量保留在 `target/frd-packed-evidence/`，哈希清单为 `EVIDENCE-SHA256SUMS`。下一阶段是 RDP6 planar 数值叶子，随后才是独立 oracle 与完整 parser/session 接线。


## RDP6 planar 叶子与 OHOS 目标执行（A2.2）

`9fe010b` 新增 checked `PlanarLayout`、planar delta、RGB 三平面打包和 YCoCg→RGB24。维度、乘法溢出、颜色损失级别和全部长度先验证再写；CLL 先在 u8 截断再有符号扩展，保留 alpha 标志对应的 R/B 顺序，奇数尺寸色度按实际 2×2 单元复制。NEON/SSE2 的完整块与数值短尾共用 ISA 原语，pack 与 chroma duplication 也使用 SIMD，没有新增正常依赖或 scalar fallback。

固定 64 条原版独立 wire/输出记录（116497 bytes，SHA-256 `32aff1b11ae76167f9dae70d8c244468ab04fbb67f6f72bbd9e7d63fdd44c9a7`），原始结果由未 patch 的 `ironrdp-graphics=0.9.0` 公共 decoder 生成，不调用新内核。测试另有独立公式和手写非灰判别值；包括全部 65536 个 delta/previous 组合、CLL1..7、alpha/子采样、121 个尺寸组合、非法输入原子性及各平面的保护页边界。未来全局 patch 前，仍须将 oracle 移到独立 workspace/process，避免同源比较。

最终显式 Rust 1.96 宿主 debug/release 各 25 项通过，3 个 benchmark 默认 ignored；fmt/clippy 通过。前期 PATH 曾命中 Homebrew 1.98.1，不能混称固定工具链结果；最终正确性回归、优化对象与基准均重新使用固定 1.96。八个受支持架构完成优化对象和 ISA 检查，未生成 macOS Intel。实际宿主 release 产物的完整块和短尾调用均对应向量数学与 pack 指令。

主代理用官方 SDK linker 完成 OHOS ARM64 release 的真实链接，随后通过 HDC 对两台专用模拟器分别执行同一组新产物，发送前及设备端 SHA-256 一致。每台原 RemoteFX 12 项、packed Bitmap 7 项、planar 6 项合计 **25/25**；这属于目标数值程序执行，尚不是普通 HAP 内完整 Bitmap/RDP6 会话接线。

| 新增目标程序 | SHA-256 |
| --- | --- |
| bitmap parity | `460a2418ced60337f902ef0803f568b09df29fb8f1fc8c46cc5fcbf8d1b643ff` |
| bitmap guard pages | `9a2a469339fe8d5a08e17824d96195b442ec0aeba23658a031e19c68b3cd7a47` |
| planar parity | `297c25cf6e24bd2dd06fab452bd36414cadaaf0946149b4ff5a72d605af16916` |
| planar guard pages | `8b6ab408a55576209a1e75d477b8dda746c2a276de1aa69be2ebe9e8ff4c3cc8` |
| bitmap benchmark | `9e13dbb1524bfb789bec1d866835746f2f51b59bce0f5eeb80c3bd65b2237c3b` |
| planar benchmark | `8e344ebde19a3d00b215acfa85d0febb1463275d65bc340d0c34aa53082f3ce0` |

停止构建与其他基准后，两台顺序运行相同预分配叶子基准，128 warmup、501 samples、每样本 64 次；对照是可能自动向量化的普通优化 Rust。以下为一次有界目标测量，单位 ns，未绑定 CPU/频率，不作整 codec 性能推断：

| 4096 pixels 操作 | 手机 native median / p95 | 手机对照 median / p95 | PC native median / p95 | PC 对照 median / p95 |
| --- | --- | --- | --- | --- |
| RGB pack | 521 / 563 | 147 / 178 | 526 / 646 | 147 / 203 |
| delta（包含重置） | 240 / 512 | 160 / 162 | 236 / 275 | 158 / 172 |
| YCoCg full | 1180 / 1400 | 351 / 397 | 1149 / 1246 | 351 / 391 |
| YCoCg subsampled | 1439 / 1697 | 6645 / 7597 | 1440 / 1891 | 6662 / 7318 |

子采样 YCoCg 对此对照约快 4.62 倍，其余三类较慢。在 packed BGR24/1024 中，native fill/XOR/dither 相比对照约快 13–16 倍，copy 仍较慢；短尾原语固定调用与 checked 边界有开销。这些差异完整保留，不能把“已使用 SIMD”写成“整体更快”。宿主数据在 `target/frd-planar-evidence/`，OHOS 构建日志、产物/哈希、两台全部测试和基准在 `target/frd-planar-runtime/`。

`9fe010b` 独立任务审查为 Spec compliant / Approved，未发现 Critical/Important/Minor。审查逐项核对源合同、原版 fixture、宿主测试和指令证据；上述 OHOS 执行为主代理另行补充，不冒称审查者操作设备。后续必须先隔离 oracle 依赖，再接入生产 parser、状态和 publication，不能以叶子通过跳过这一层。


## A2.3 原版对照隔离与固定生产副本

`1455c2d` 将原版 decoder 运算移至 `tests/legacy-oracle` 独立 workspace/process：graphics0.9.0、session0.11.0、PDU0.9.0、yuv0.8.17 使用精确 registry pins 和自有锁。主代理在添加全局 patch 前后核对完整依赖图：生产 session → 固定 graphics 副本 → kernel；kernel 包括 dev 边也不再依赖原版 decoder，独立 oracle 仍只使用 registry 原版。两个生产副本的 decoder 源码保持原字节，只有 graphics manifest 增加私有 kernel 依赖；当前尚未调用新内核。

独立进程冻结25,962个叶子输入键，保留全部4096个DWT脉冲及原极值/随机覆盖；原planar64输出哈希保持不变。另固定22,337条完整wire记录（Bitmap16,721、Planar4,972、RLGR634、Session10），记录公开API实际结果、完整输出或错误后的部分输出；没有猜测不可观测的内部消费数。全部546个成功RLGR输出另存为5,839bytes压缩夹具，恢复原有数值zero重放覆盖。该重放仍不是parser内的SIMD调用。再生成和来源核验方法见[独立原版对照说明](../../tests/legacy-oracle/README.md)。

宿主kernel debug/release各23项通过：原25项中的两个原版parser/session测试迁到独立runner，未删除语义覆盖。独立runner debug/release各6项通过，最终debug/release重新计算全部原版输出并逐字节比较通过；补齐zero重放后的受影响测试另行通过。RDP回归321 passed /3 ignored，根locked check通过。新加入的graphics→kernel依赖当前尚未消费，因此协议构建有一项新增unused-crate-dependencies警告；现有dead-code警告另行保留，未宣称构建无警告。excluded依赖的upstream套件不能直接从根workspace执行，Cargo拒绝该尝试，未将其计为通过。原样vendor三处文本的空白告警保留以维持源哈希。

基准拆为独立进程后，双方新增相同“系数复制起点”的component/tile计时。一次本机安静窗口中，中位数component为native8145ns/原版4828ns，tile为31830ns/15898ns；原版包含RLGR的完整wire指标另行保留，不能跨范围相除。此处是测试拓扑和计时范围的验证，不是新的优化收益或OHOS性能证据。此前手机/PC25项结果属于A2.2旧测试产物，本轮未重新执行目标程序。

独立任务审查为 Spec compliant / Approved，无 Critical/Important；两项 Minor 分别是过时的 patch 说明和警告披露，均由文档修正处理。完整Bitmap、RDP6、RLGR/RFX parser与会话publication、像素转换和目标包/实时连接仍是后续工作。

## A2.4 Bitmap parser native consumer

`484d1b2` 以一个共享 parser 保留原入口行为，新增显式 `decompress_native(..., &NativeKernels)`，在实际解析时调用 checked fill/copy/XOR/mask/dither。命令级 first-line 特例、FG源长度检查、mask跨行位偏移、555 WHITE、错误先后和失败前已写部分输出保持。Native入口检查几何溢出并分别报告RLE/Kernel错误，不静默重试scalar。当前session仍使用原默认入口。

Host macOS ARM64 原版16,721 catalog与另7,616固定原版源码生成的畸形输入全部逐字节/结果一致；121,856偏移canary、80命令族深度组合/224header行变体通过。实际生产rle.rs的debug strictclippy及opt3/thinLTO release各7 tests、kernel Bitmap7、RDP321/3ignored通过。独立registry oracle保持，与K无依赖循环；显式FRD-MODIFICATIONS.json固定修改后sourcehash，原source/archive检查仍严格，6个来源mutation测试通过。没有让期望值来自生产实现。

独立审查 Spec compliant / Approved，无Critical/Important；protocol既有25个clippy及4个test警告均在未改文件，不能称整包零警告；rle strictclippy本身干净。生产source SHA256为`43a90ca26fb4611395bbc78e9f6929d9797b96a35e2c26d1d7ab879ab46f1226`。完整报告与日志保留在`.superpowers/sdd/2026-10-13-harmony-clients/task-4a24-*`。本任务没有OHOS HAP/runtime/Windows实连证明，剩余RDP6、RLGR/RFX、像素转换和session接线继续受门禁约束。

## A2.5 RDP6 parser native consumer

`0f207c8` 让共享的 RDP6 plane parser/controller 在显式 native 入口中调用 checked copy/fill/delta/RGB/YCoCg 叶子；原入口默认保持。Alpha RLE 必须完整解码并消费，raw alpha 跳过完整平面；CLL0+CS 仍为ARGB，奇数chroma尺寸用ceil。三平面全成功后追加RGB，prefix保留；parser失败允许原私有scratch变化，Kernel错误分别携带来源，输出新增部分回滚且不回退scalar。零尺寸、乘积/isize、append和header特定aggregate/reservation失败走显式安全错误，不能说成原版panic或wrap行为相同。

固定原版4,972 catalog（552成功/4,420错误）和4,181补充（950成功/3,227错误/4个64bit原panic），另六个独立BMP RGB。6,906个普通bitmap字节/错误parity与16个geometry安全拒绝分开；2,237个私有plane病例测实际成功consumption、partial和16偏移canary，bitmap及error的不可观测消费数不伪造。i686四个原64bitpanic只作native安全classification，不外推panic相同。宿主debug/release/strictclippy各20/20、Bitmap7、RDP321/3ignored及三变异控制通过；独立审查 Approved，无Critical/Important。四条既有RDP dead_code警告保留，新增scopedclippy干净。

Root使用`83ec96e`隔离gitarchive加全部且仅15个`0f207c8`提交文件，保存每个overlay hash，再以官方API26 NDK和Rust1.96 offline/locked构建实际shipping graphics与相邻RDP库。新消费者构建5.74秒通过。私有harness复用作者的实际源码测试与Cargo-built library public API，由Cargo JSON精确识别五个OHOS rlib；首次direct rustc未加入host proc-macro搜索目录而E0463，补充该目录后成功，未改产品源码。目标ELF只依赖系统libc.so，SHA256 `e49b21f96a7d10a2d3cb3f7950525e3f7dd796c186e9896756d39a619a704e67`。发送前与guest SHA相同，手机/PC API26 ARM64各20/20、实际exit0，PC约0.49秒。

原expect-test/bmp dev依赖缺offline缓存，因此15个原RLE/encoder测试使用严格Snapshot assertion adapter，不能宣称原framework或全上游suite运行。目标harness为debug正确性程序，没有目标优化性能、普通HAP解码接线、Windows/Linux native执行或真实RDP证明。现有登录包和UI未改变；session/default/native production eligibility仍待A2.6/7。全部artifact身份、命令、库哈希和两台原始日志在`target/harmony-rdp-platform-check-83ec96e/`，宿主记录在`target/task-4a25-*.log`。


## A2.6：实际 RLGR/RFX 与会话原生消费者

2026-10-13，`23ea916` 让同一 RLGR parser 在真实零游程和末尾补齐处调用已有原生 zero；不扫描/重放解码后的输出。Checked component 在首次写入前验证两个4096缓冲区，按明确十band量化映射执行 LL3 4032..4096 differential、dequant、DWT；三组件和checked color均成功后才apply当前tile。Native失败保留来源，不回退scalar；第二tile失败保留此前已apply的第一tile，不承诺整帧回滚。

构建器字段与默认None保持。显式Stage/Processor/Context setter传入中立能力，Stage替换Processor时保留选择并传递到新RFX context；Bitmap fast/slow共享同一consumer。Native Bitmap错误在imageapply/Region前返回，默认的历史warn+rectangle行为保留。生产工厂和codec广告尚未切换，image.rs仍为下一步像素转换门。独立审查为Spec compliant / Approved，无Critical/Important/Minor。

宿主ARM64 debug/release各core3/session4、scoped clippy通过。独立原版冻结1471 RLGR、30组件及28实际会话记录；包括2970byte wire/16384byte原图、裁剪与两个tile错误发布。最初HL/LH互换故障无法被相同shift的量化fixture区分，增加6条独立原版q=[2,3,...11]记录后，六个语义故障变体全部被拒绝，旧fixture不改。原Bitmap7、RDP6 20、RDP321/3既有ignored以及来源/依赖隔离检查通过。完整命令与边界见vendored session [测试说明](../../third_party/ironrdp-session-0.11.0/tests/README.md)。这不是完整上游dev-dependency suite。

Root在既有83ec96e隔离源码加0f207c8 overlay上，仅加入全部24个冻结23ea916文件，保存逐文件SHA；没有编译并行A2.7A工作树。官方API26 NDK/Rust1.96 offline/locked实际shipping库构建13.74秒通过。最初验证环境PATH漏掉Homebrew cmake而构建失败；补充工具路径后通过，未改产品源码或依赖。Cargo JSON精确识别目标rlibs；实际RLGR/bridge源码及实际session全模块harness分别链接这些库，两份ELF都仅依赖系统libc.so。

- core ELF SHA256：`6d5eacec7039a98876512698f1547d9f86232c92a80414c6725f55b82cec9034`。
- session ELF SHA256：`6208f0e845ed5311ade3791262893d4b210c3a915e2dcbf4b9be85cb47bd0745`。
- 手机API26 ARM64：core3/3、session4/4；PC API26 ARM64：core3/3、session4/4。发送前后hash一致，各程序有实际FRD_EXIT=0标记。

原始overlay/build/harness身份、两端stage与test日志及`a26-guest-results.json`在`target/harmony-rdp-platform-check-83ec96e/`。这是actual-source/shipping-library数值与会话夹具运行，不是普通HAP、真实Windows登录、端到端延迟或Windows/Linux架构运行。已安装正常登录包未替换，开发验证入口没有恢复。

生产选择前仍须补齐Bitmap EOF underfill的真实written计数（预resize后的len不能证明完整）、声明源尺寸与目标矩形的明确stride/crop/padding、八格式row/image/cursor写入，以及每个目标的运行和包门。当前成功Bitmap夹具完整且尺寸相同，不能借Region声称全幅baseline准备成功。原版非法量化/色彩溢出的panic或wrap与native typed拒绝单独分类。
