# RemoteFX 原生数值叶子

独立 checked backend；当前没有任何生产调用者、graphics patch、能力广告或 session 就绪接口。正常 dependencies 为空。registry `ironrdp-graphics =0.9.0`、`yuv =0.8.17` 和 `ironrdp-session =0.11.0` 仅为未修改的 dev oracle。完整协议接线、Bitmap/RDP6、平台包和 live 验证仍未完成。

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

`cargo test --locked -p frd-rdp-legacy-kernels` 对比原版 oracle，覆盖全部 4096 位置 DWT 极值脉冲、原版 DWT/RLGR/完整 session wire fixture、quant 各 band、任意尾部与非对齐 slice、色彩全 i16 域抽样与合法域边界、失效能力和错误不修改输出。

`wire_composition` 用原版 parser/RLGR 和新数值叶子测试期组合得到完整 tile，另与未修改的 session decoder 及原版期望图像逐字节比较。RLGR zero-run 回调尚未接入：测试只是按原版 decoder 产出的零区间重放独立 materialization。截断 bit 流按原版规则可成功并补零；空 tile 返回原版 `EmptyTile`。编码器字节末尾 padding 在输出长度被人为扩大时可能解出额外非零符号，不能臆定全部多余样本为零。wire 截断拒绝由未修改原版 session 测试保留。这些均不是生产 wiring、错误映射、发布原子性或 live 互操作证明。

显式 release benchmark：`cargo test --locked --release -p frd-rdp-legacy-kernels --test benchmark -- --ignored --nocapture`。固定 wire SHA-256，128 warmup、501 samples（每个样本批量 16 次以避免时钟分辨率导致零值），报告 median/p95 ns、backend、host 架构、oracle/native 比值；不设置性能断言。预分配 scratch 和输出，dequant/DWT 计时包含输入重置 copy，color 包含 checked 预检。component/full-tile 包含未修改 RLGR 的 parser **及其原版零 materialization**；zero leaf 单独测量，不能将组合计时称为全部 SIMD 的生产 decoder。

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
