# RemoteFX / Bitmap / RDP6 原生数值叶子

独立 checked backend；正常 dependencies 为空。生产 graphics/session 已固定为最小 vendored 副本，graphics 私有依赖本 crate，但尚无 decoder 数值调用者或能力广告。原版 graphics/PDU/session/yuv 位于 `tests/legacy-oracle` 的独立 workspace/process，kernel 的 dev 图也不再含这些 decoder。完整协议接线、平台包和 live 验证仍未完成。

`NativeKernels::detect()` 仅在 AArch64 NEON 或非 macOS 的 x86/x86_64 SSE2 可用时构造能力对象，其他 CPU 返回 `Unavailable`。32-bit x86 与 x86_64 使用分别 cfg 的 intrinsics import；只用 SSE2（`loadu`，用两次 `mul_epu32` 拼低 32 位乘积），不需要 SSE3/SSE4.1。macOS Intel 不提供 backend。`detect_if(false)` 可收紧策略，`true` 仍须通过真实硬件检测。没有 scalar 后备。OHOS 的 Linux OS cfg 不加载任何桌面服务。

## checked API

| 方法 | 输入、算术和错误 |
|---|---|
| `dequantize` | 恰 4096 个 i16；quant 按 HL1,LH1,HH1,HL2,LH2,HH2,HL3,LH3,HH3,LL3 顺序。0/1 保持原样，2..=16 左移 q−1 后保留低 16 位。全部 quant 预检；>16 返回 `InvalidQuant`，不模仿上游 debug 的超位宽移位 panic。wire nibble 最大 15，额外接受 q16 是定义良好的上游叶子域。 |
| `inverse_dwt` | 输入和独立 scratch 均恰 4096 个 i16，否则 `Length`。三层 offset/width =3840/8、3072/16、0/32；加法在 i32 中做，偶数结果存回 i16；奇数高系数先在 i16 左移截断，再扩展；各层边缘复制。 |
| `differential` | 任意长度（含空）modulo16 inclusive prefix；调用方选择 LL3 通常 64 样本。每个向量和尾部都用 SIMD，跨向量 carry 是最后一个已截断 i16。 |
| `zero_i16` | 任意长度（含空）的 SIMD 零 materialization。完整向量用显式原生 vector store，防止 LLVM 折叠成未经验证的 libc memset；短尾使用本地定长向量；不解析 RLGR。 |
| `ycbcr_to_rgba` | 三个平面长度完全相同，输出恰为 4×样本数（checked_mul），含空；不满足返回 `Length`。连续一维像素，调用方逐行处理 stride。RGBA alpha=255。以下每个算术节点先检查，任一失败返回 `ColorOverflow` 且整个输出保持原值。 |

所有错误发生于调用方输出被修改之前，包含 DWT scratch。可变/不可变 slice 的 Rust 借用规则禁止非法别名；没有外部 padding 要求。架构数组 load/store 只接触本地 staging 或已检查的完整数组。DWT gather/scatter 仅搬运布局，系数计算在 SIMD 中完成。

色彩精确合法域遵循 yuv 0.8.17 原表达式，而非猜测固定窄范围：`y + 4096` 必须能用 **i16** 表示；其扩展后的 `yy=(y+4096)*65536` 能用 i32 表示。`91915*cr`、`46818*cr`、`22526*cb`、`115992*cb` 各自能用 i32 表示；`yy+91915*cr`、`yy-46818*cr`、该结果再减 `22526*cb`、`yy+115992*cb` 各自能用 i32 表示。每通道算术右移 21 后 clamp 到 0..255，**没有 rounding bias**。预检与转换均用 SIMD，两遍遍历避免分配整帧临时输出。

候选研究中 FreeRDP 常量 91916/46819/22527 不能直接用作当前 oracle 常量。yuv 的 f32 常量乘 65536 再截断实际为 **91915/46818/22526/115992**；`[-8265,8234,-20324]` 必须输出 `[0,234,255,255]`，错误常量会使 G=235。上游对合法域外输入的 debug panic/release wrap 差异不定义本 crate 的行为；这里统一返回明确错误，后续生产错误映射另行审查。

## 测试与验收边界

`cargo test --offline --locked -p frd-rdp-legacy-kernels` 对比独立原版生成的冻结逐例 bytes，保留全部 4096 位置 DWT 极值脉冲、12 random seeds、5 constant extremes、170 dequant cases、prefix/tail/canary 与 100000 组色彩全 i16 域抽样及全部原错误门。`tests/common/frozen.rs` 只做带输入 SHA-256 的查表，没有 decoder 公式；gzip 只在 dev 图加载一次。fixture 生成与独立重算见 [`tests/legacy-oracle`](../../tests/legacy-oracle/README.md)。kernel 测试可以在目标运行，不启动外部 Cargo/oracle binary。

`wire_composition` 使用原版完整 wire 捕获的 RLGR 系数/quant 做数值叶子组合，并逐平面对照原版 DWT 输出及 16384-byte 原始图像。零区间重放保留原有边界，不是 entropy zero hook。原版真实 RLGR、完整 2970-byte wire/session、truncate/error/padding 测试已迁到独立 runner，并新增完整 wire catalog；kernel 不将 patched parser 当 oracle。原版逐字节消费未由 public API 暴露，catalog 不臆测该数值。独立原版 debug panic/release wrap 的色彩测试同样保留。这些均不是生产 wiring、错误映射、发布原子性或 live 互操作证明。

显式 release benchmark：`cargo test --offline --locked --release -p frd-rdp-legacy-kernels --test benchmark -- --ignored --nocapture`。原版使用 `cargo test --offline --locked --release --manifest-path tests/legacy-oracle/Cargo.toml --test benchmark -- --ignored --nocapture`（仓库根执行并设不同 `CARGO_TARGET_DIR`）。双方相同输入 hash、thin LTO、128 warmup、501 samples×16；报告 median/p95、架构和 profile。dequant/DWT/LL3 包含相同输入 reset copy，color 包含 native checked 预检。新的 `component_*_coefficients_4096` / `tile_*_coefficients_3x4096` 从相同冻结系数开始且都包括 copy，适合跨进程同边界比较；fixture 加载/解压/进程启动不进入计时。独立原版仍报告含 RLGR 的原 `component_oracle_4096`、`tile_oracle_3x4096` 与单独 RLGR 计时。旧 mixed native/fullwire 历史结果保留历史含义；不得将新 native coefficients 数值除以 original fullwire 数值。真正全 native wire benchmark 等待 A2.6 parser 接线，不设置性能断言。

跨目标编译不代表目标运行：Windows/Linux x86、x86_64、ARM64，OHOS ARM64 各自需要执行 parity/benchmark、包与 runtime 门。macOS 只覆盖 ARM64。native host 数值/指令证据同样不证明任何客户端能控制目标系统。

## Packed Bitmap run 叶子（A2.1，2026-10-13）

`PackedDepth::{Index8,Rgb555,Rgb565,Bgr24}` 分别为1/2/2/3个原始小端字节。555的高位也原样保留；WHITE由parser决定，555 `[ff,7f]` 与565 `[ff,ff]` 不合并。以下API只接受已构造的 `NativeKernels` 能力对象，任何错误返回 `Length` 并保持整个dst原样。

| 方法 | 契约 |
|---|---|
| `copy_u8(src,dst)` | 两slice长度相等；任意字节长度，含空。 |
| `fill_pixel(depth,pixel_le,dst)` | pixel长度精确等于depth；dst必须是完整pixel数；重复原始packed值。 |
| `xor_previous(depth,previous,fg_le,dst)` | previous与dst长度相同，fg长度精确；逐原始字节XOR同一FG。 |
| `mask_previous(depth,previous,fg_le,mask,bit_offset,dst)` | previous=None是caller决定的命令级first-line零背景；Some长度与dst相等。LSB-first，每pixel选择background或background XOR FG；checked `bit_offset+pixel_count<=8`。空目标仍检查offset，8有效，9..255错误。 |
| `dither_pixels(depth,a_le,b_le,starts_with_b,dst)` | 两pixel长度精确；交替重复，caller按已发pixel奇偶传相位；odd split必须切换相位。 |

长度和mask范围在首次输出写入前全部检查。借用不重叠；上一行依赖、跨行命令first-line状态、setFG、wire pair数量和dither相位属于未来parser caller。没有对whole parser scratch回滚的承诺，也没有生产注册或协议广告。

实现与RemoteFX分文件：`bitmap.rs` 准备最多48bytes周期pattern，三个16byte相位处理packed24和6byte dither；一个mask最多准备24bytes。其后重复输出、copy和XOR均由 `bitmap_neon.rs` 的显式 `ldr/str q`、`eor v.16b` 或 `bitmap_x86.rs` 的显式SSE2 `movdqu/pxor` 完成。两种x86架构分别编译；不依赖SSSE3/SSE4。显式assembly阻止小case或bulk被LLVM替换成scalar/libc。短尾在本地16byte staging上执行相同vector操作，再只搬运合法字节；不足16byte的staging搬运允许标准slice copy，不作为bulk生产后备。

`bitmap_parity` 是独立test-only逐pixel模型与手写packed24/555/565预期值；覆盖全部256mask、每个有效offset/count（含空）、两种previous模式、byte offset0..15、长度0/1/7/8/15/16/31/32/63/64/65、raw555高位、odd split、canary，以及pixel/长度/mask范围错误原子性。`bitmap_guard_pages` 在Unix将输入/输出及pixel参数分别紧靠页首和不可访问页尾，覆盖长度0..65及所有有效mask。没有新增正常dependencies。

有界host benchmark：`cargo test --offline --locked --release -p frd-rdp-legacy-kernels --test bitmap_benchmark -- --ignored --nocapture`。128warmup、501samples×64batch；1/7/8/16/65/1024pixel和1/7/8mask。预分配输出，native包含checked入口/pattern准备；reference为普通优化Rust逐pixel循环和标准slice copy，不含checked入口；compiler可能自动向量化reference。输入和输出在调用边界用black_box，输出逐字节验证。报告median/p95，无性能断言；这不是原版Bitmap wire parser或whole-codec成本。

本次macOS ARM64/Apple M4 debug与release packed及原A1测试通过，优化object中的完整块和短尾NEON指令已核对。Windows/Linux x86、AMD64、ARM64及OHOS ARM64仅完成Rust1.96优化object交叉编译与指令检查，未在这些目标执行parity/benchmark，未链接平台应用或验证package/runtime/live互操作。未来目标执行和完整Bitmap/RDP6/session门仍须单独完成；macOS Intel不支持。

## RDP6 planar 叶子（A2.2，2026-10-13）

当前仍没有 production dispatcher。`PlanarLayout` 包含非零 `width/height`、`subsampled`、`color_loss_level` 和 `has_alpha`。入口在任何写入前检查尺寸乘法、输出乘法、ceil chroma 尺寸、CLL 和全部 slice 长度；零尺寸或尺寸溢出返回 `InvalidDimensions`，CLL 非 1..=7 返回 `InvalidColorLossLevel`，slice 不匹配返回 `Length`。错误保持整个 output/current 原样。

| 方法 | 精确契约 |
|---|---|
| `resolve_planar_delta(previous,current)` | 两 slice 长度完全相同，含空。偶数 d 转为 d/2，奇数转为 255−(d−1)/2，再与 previous 模 256 相加；没有 saturation 或水平 prefix。caller 只对非首行调用。 |
| `rgb_planes_to_rgb24(r,g,b,out)` | 同长度三 planes，输出恰 3×len，含空；输出 `[R,G,B]`。没有 stride、alpha 或 subsampling。 |
| `ycocg_planes_to_rgb24(layout,y,co,cg,out)` | Y 恰 width×height，Co/Cg 恰完整尺寸或 ceil(width/2)×ceil(height/2)，out 恰 3×full。Co/Cg 先在 u8 域左移 CLL−1 截断，再 cast i8 扩展到 i16；t=Y−Cg，R=t+Co，G=Y+Cg，B=t−Co，每通道 clamp0..255。has_alpha=true 输出 `[R,G,B]`，false 输出 `[B,G,R]`。 |

所有数值中间值在 i16 内有界；无 alpha 数值输入，alpha plane 的读取、解码、丢弃仍是未来 parser caller 的责任。subsampled chroma 每 2×2 pixels 复用，奇数最后 row/column 只输出合法像素。没有全尺寸 chroma 分配；逐行处理最多 8 pixels 的本地 staging。

`planar_neon.rs` 提供 16-byte delta、8-lane u8 shift/signed-widen/i16 add/sub/saturating-narrow、NEON `zip1` chroma duplicate 和 `st3` RGB24 pack。`planar_x86.rs` 以 SSE2 word shift 后 low-byte sign extension、`paddb/paddw/psubw/packuswb` 实现同一语义；RGB pack 用 SSE2 unpack、byte shift、mask/OR，不依赖 SSSE3。两个 x86 架构分别 cfg/import/编译，macOS Intel 没有 backend。固定 lane primitive 不内联，完整块与尾部 staging 调用同一 ISA 路径；只在有限数组间搬运布局 bytes，不以 scalar color/delta loop 作为生产后备。

`planar_parity` 覆盖 delta 全 65536 pairs，CLL1..7 × alpha × subsampling，11×11 dimensions（1/2/3/7/8/15/16/17/63/64/65），所有 byte offset0..15、RGB marker、尾部 canary、尺寸/CLL/长度错误原子性。`planar_guard_pages` 将每个独立 plane/output 紧靠不可访问页首/页尾，覆盖 empty RGB/delta 和完整块/短尾；不要求外部 padding。`tests/fixtures/planar-original.bin` 是未修改 registry IronRDP decoder 实际输出的 64 条固定 records，包含 28 色彩配置的6×8×8判别值与3×3各 chroma cell 不同的奇数尺寸、ARGB marker 与六条 RLE delta plane。生成源码、hash、格式与来源见 `tests/fixtures/planar-original-provenance.json`。kernel tests 对照全部固定 bytes；独立 oracle runner 重新生成并逐字节验证原版 64 records，原始 bytes/hash 未变。全局 patch 不进入独立 oracle。

显式 bounded host benchmark：`cargo test --offline --locked --release -p frd-rdp-legacy-kernels --test planar_benchmark -- --ignored --nocapture`。128 warmup、501 samples×64 batch，1/7/8/16/65/1024/4096 pixels。双方使用预分配 output，delta 双方每次重置输入；native 包含 checked 入口，reference 是普通优化 Rust 数值/布局循环，compiler 允许优化或自动向量化。报告 median/p95，不设性能断言，不代表原版 parser/RLE/whole-codec/session 成本。

macOS ARM64/Apple M4 已执行 debug/release parity、guard 与 benchmark，优化对象中的实际 NEON primitive/调用路径已检查。Windows/Linux x86、AMD64、ARM64 和 OHOS ARM64仅交叉编译优化 object 并核对 SSE2/NEON 指令；没有目标执行、per-target benchmark、应用链接、包或 live 互操作证明。完整 RDP6 alpha/RLE/parser 消费与 session publication 接线仍待后续独立 scope。

## 独立像素行转换（A2.7A，2026-10-13）

`convert_pixel_row(source_format,destination_format,source,palette,destination)` 仅转换一条紧密行；不接受调用方 offsets、padding、stride、rect、cursor 或协议类型。`Pixel32Format::{ARgb,XRgb,ABgr,XBgr,BgrA,BgrX,RgbA,RgbX}` 描述内存字节顺序。源为 `Packed(PackedDepth)`、`Rgb24`、`Raw32(Pixel32Format)` 或 `Region32(Pixel32Format)`。

入口先检查完整源像素与 checked `pixel_count*4==destination.len()`，错误为 `Length`；其后 Index8 检查归一 `[[R,G,B];256]` palette，缺少为 `MissingPalette`，包含空行。其余源格式忽略 Some palette。合法空行成功，任何错误保持全部目标 bytes，生产不分配内存。

| 模式 | 第四逻辑通道 |
|---|---|
| Packed / RGB24 | 所有 A/X 目标均为255。555/565 LE 输入保留原版精确 `(c5*527+23)>>6` / `(c6*259+33)>>6`。 |
| Raw32 同 enum | 全 byte 原样 copy，包括 X padding。 |
| Raw32 不同 enum | A 源保持 alpha，X 源为255；即使目标为 X 也写入该值。 |
| Region32 同字节序 | A/X 配对也全 byte 原样 copy。 |
| Region32 不同字节序 | read_color→write_color；仅 A→A 保持，X 源或 X 目标均为255。 |

同格式/同字节序整行 copy 复用现有 checked ISA `copy_u8`。其他完整四像素与短尾均调用同一 NEON/SSE2 core；尾部仅使用16byte输入/输出 staging。555/565 用 vector word multiply/add/shift，24bit 用 NEON table 或 SSE2 byte mask/shift 解交织，32bit 用 vector swizzle。Index8 的索引、地址计算、16bit RG 与8bit B读取和 canonical RGBA组装均在目标 inline assembly；index255不会读取第768byte。NEON opaque格式只做一次固定 TBL，SSE2仅用固定mask/shift/OR；RGBA/X直接返回 canonical向量。x86/i686 和 AMD64 分别 cfg，不要求 SSSE3，ARM64不使用x18。inline assembly由编译器管理 ABI寄存器保存，不能把对象编译视作另一 OS 的运行证明。

`tests/pixel_original` 保存原版 image.rs / image_processing.rs / color_conversion.rs 完整、原样源和独立 rustc process wrapper。archive/member hash 与独立原版 registry rlib/depfile hash 均记录；不修改或继承根 patch，K正常和dev图没有这些decoder。生成器可在固定本地 original artifact 存在时用 `python3 crates/frd-rdp-legacy-kernels/tests/pixel_original/generate.py` 重算；普通测试只读取已冻结、hash核验的6,453,432byte期望（gzip3.3MiB）。176records包含555/565各65536×8、两组非灰palette所有256索引、Raw32/Region32各64组合和六个alpha锚点。QOI仅单独编译stock feature=qoi RGBA helper，证明映射Raw32(RgbA)的八格式alpha；不代表QOI wire解码。

`pixel_parity` 测全部格式的短尾、source/destination byte offset0..15、canary、错误零写入和手算锚点。`pixel_guard_pages` 在Unix用真实不可访问页包围source/destination，并将只读palette末项紧贴保护页；原 `pixel_abi` outer callback会恢复内层破坏的x25，不能作为完整ABI证明；2026-10-14修复后，private unit test `native::pixel::abi` 以精确Rust function pointer直接包围实际`arch::block`与selected `k.copy`。probe在target返回后立即捕获x19..28、d8..15 low64和x29 frame-mask，再恢复caller；没有Rust bridge夹在target与检查之间。执行前debug/release反汇编核验w0/w1 source-tag/payload、w2目标、x3源、x4palette及x8 SIMD sret，copy为x0..x3双slice，全为register-only。三个负控制实际调用原core/copy后故意破坏x25或x29，必须分别检出64/262144；正例覆盖176records、4224core、144selectedcopy及1056publicrows。执行命令为 `cargo test -p frd-rdp-legacy-kernels --lib native::pixel::abi --offline --locked`，release加`--release`。scope只证明这些macARM测试binary的真实callee边界，不代表其他OS/ABI运行。Rust1.96固定工具链已运行macARM debug/release K全集与strict scoped clippy；八个既有非macIntel目标只完成优化对象/反汇编，Windows/Linux/OHOS未执行本叶子的native parity或ABI。

`pixel_benchmark` 为须协调quiet window的显式 ignored release host基准。原版另process使用预分配stock单行apply（pointer hidden），Region32调用stock ImageRegion::copy_to；native包含public checks、gather、ISA调用与tail staging。双方176records×1/3/4/65/1024/4096长度，32warmup、51samples×16calls，逐例输入和输出SHA256必须相同。stock image check/隐藏pointer bookkeeping与native leaf成本边界不同，不是whole-codec或session性能结论。Index8和raw-copy慢项及优化前后数据见ignored实施报告；保留剩余慢项而不以大类median隐藏慢case。A2.7B image/padding/cursor/session接线、目标包/runtime/live门仍由主代理单独验收。
