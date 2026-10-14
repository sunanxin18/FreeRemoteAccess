# 私有 HEVC16 原版 C/reference 门

状态：**SOURCE-READY；零次编译、零次链接、零次测试执行；尚未接受**。本文件和
`hevc-intra16-reference.c` 不进入 FFmpeg build、source recipe、HAP 或产品默认。
Task4C5D1A 先建立原版 C/model 成功和真实 selector RED，root 明确解除执行冻结并
完成这两个门后，才允许另一步 D1 planar/angular NEON 汇编。源码存在不证明新核资格。

## 原版身份及真实 API

固定归档：FFmpeg 8.1.2，SHA256
`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`。
归档本轮没有重新读取；以下三个文件已在两份可读树重新计算 SHA256，一致：

| 原版文件 | SHA256 |
| --- | --- |
| `libavcodec/hevc/pred.c` | `27eab80002d493efadadbe55309d3f2be7359c24d349f582edf3d2ffbb35f9e2` |
| `libavcodec/hevc/pred_template.c` | `70fe448519c1806d416a45afe5e4fb89ec8269809b606ac2821e30b67d82788a` |
| `libavcodec/hevc/pred.h` | `95a968528689ee80f528c6941004a6f63ed450e4388a4049ed237d00fed3412f` |

两份树分别为 `target/ffmpeg-ohos/attempt-qjsezbh7/ffmpeg-8.1.2` 和
`target/task4c5c1-runtime/ffmpeg/build.V9P45r/source/ffmpeg-8.1.2`。均只读，
没有复制、修改 template，也没有重新构建 FFmpeg。

`HEVCPredContext` 完整字段是 `intra_pred[4]`、`pred_planar[4]`、`pred_dc`、
`pred_angular[4]`。原版 `ff_hevc_pred_init(HEVCPredContext *, int)` 对 9/10/12
使用各自 C 表，所有其他位深（包括 0/7/16）使用 8-bit C 表；原版只有 MIPS override。
16×16 是 planar/angular `[2]`，不是 HEVCDSP 的 IDCT/MC/filter context。

```c
void (*pred_planar[4])(uint8_t *src, const uint8_t *top,
                       const uint8_t *left, ptrdiff_t stride);
void (*pred_angular[4])(uint8_t *src, const uint8_t *top,
                        const uint8_t *left, ptrdiff_t stride,
                        int c_idx, int mode);
```

本门调用仅为 8-bit16×16，stride 单位为 bytes。9/10/12 只做完整 context
初始化和分派检查，没有运行其像素 callback。`intra_pred` 的原 `HEVCLocalContext`、
`HEVCPPS` 等完整定义通过原版 `hevcdec.h` 解析；不构造假的上下文，不调用邻边准备。

用同一私有文件的 `FRD_HEVC_ORIGINAL_TU` 编译形态，将未改 `pred.c` 直接 include，
只通过宏把 `ff_hevc_pred_init` 政名为 `frd_hevc_original_init`。该 TU 实例化原版
8/9/10/12 callbacks 和 init。正常 harness TU 引用实际 `ff_hevc_pred_init` 的
现成原版 `pred.o`；这两个 TU 的静态 C callbacks 地址本来就可以不同，因此从不
跨 TU 比较函数地址来证明 NEON 分派。像素比较以单独写的整数模型为第三方 oracle。

独立模型使用显式 `floor_div`，所有负 idx、inv-angle、luma 边界差值向下整除，
不依赖负数算术右移，也不把 C 向零除法当向下除法。原模板完全保留它自己的行为。
两份 angle 表数值已与原模板逐项静态核对。模型只是测试 oracle，不是生产后端。

## 有限且可审的 case 库

输入合法范围是各边 `[-1..31]`，共同 `top[-1] == left[-1]`，dst 不与引用重叠。
原版负角 scratch 的 `x=0,4,8,12,16` 四 byte 复制实际读取边 `[-1..18]`，
测试提供完整 `[-1..31]`，不会以太短 oracle 缓冲掩盖其合法 copy padding。
独立模型只读取数学需要的值；这不改变原版 TU 的读取合同。

图案共有 46 个，每个生成函数短小，不存储大块编码 fixture：

| id | 图案 |
| --- | --- |
| 0..5 | 常量 0/1/127/128/254/255 |
| 6..7 | 升序/降序互补梯度 |
| 8..9 | 0/255 棋盘及相反相位 |
| 10..19 | top/left 在 0/15/16/31/共同 -1 的单脉冲；共同 -1 两项有意重复 |
| 20..21 | top[16]/left[16] 相反端点 0/255，其余 128 |
| 22..23 | 一边 0、另一边 255，共同 corner128 |
| 24 | 两边 0、corner255：负奇差 -255 的下限 clipping |
| 25 | 两边 255、corner0：正奇差255 的上限 clipping |
| 26..27 | 两边 128、corner131/129：负奇差 -3/-1 向下舍入 |
| 28..29 | 15/16 与 0/1 相反相位，插值舍入边界 |
| 30..45 | xorshift32；固定 seed `0x4c5d2026`，每图案按 id 派生状态 |

每个 variant：planar 一次，angular 全部 `mode2..34 × c_idx0/1/2`，共100 cases。
正常缓冲区矩阵严格固定如下：

| 组 | variants | cases |
| --- | ---: | ---: |
| 数值组：46 图案，stride32，三个地址均 mod16=0 | 46 | 4600 |
| 布局组：图案24/29/30，8 strides，每布局先全对齐，再独立扫 dst/top/left 各15个非零 residue | 3×8×46=1104 | 110400 |
| 同址只读 top==left：46 图案，stride32，对齐 | 46 | 4600 |
| 合计 | 1196 | 119600 |

strides 固定 `16/17/31/32/64/257/-17/-31`。每个地址 mod16 的16种值逐一覆盖，
但**不是三地址16³交叉乘积**，也不是46图案×所有布局的完整乘积。同址边组不扫布局。
每例对整个4096-byte dst（含逐行 padding 与前后 redzones）逐 byte 比较，原版 C
和实际选择 callback 都必须与模型一致；两份完整96-byte input 存储必须保持不变。
没有用最终 checksum 代替这些比较。负 stride 的 row0 放在缓冲区高处，最小步长
绝对值17，行间无重叠；最大 stride257 的合法 footprint3871 bytes。

模式覆盖全部正、负、零 angle，负 -2 的 `last=-1`（mode11/25）及其余负角扩展。
每个 angle 的实际 fact 库准确限定为 `{((k*angle) mod32): k=1..16}`，余数取0..31。
±32/0 只有fact0；±2/±26覆盖全部偶数fact且包括0；其余±5/±9/±13/±17/±21
只覆盖其16个可达fact，**不声称每模式覆盖fact1..31**。没有另行枚举256²插值输入。
mode10/26各 c_idx 运行；图案24..27专门区分 luma correction、chroma无修正、
负奇差 floor 与上下限 clipping。

`FRD_HEVC_GUARDS` 是额外、可选的私有执行形态，使用冻结
`h264-luma-dc-platform.h` API，不编辑其实现。它对图案24/29/30 ×8 strides×
前/后紧贴 guard page 两侧×100 cases，共4800模型 cases、9600 callback 调用，
参考页改只读，整个页内容和 dst padding 比较。两侧分别使 `[-1]` 或 `[31]`
紧贴不可读页；dst 以整个 footprint紧贴前/后页。它不是任意地址 residue 与 guard
位置交叉乘积，也不是 ABI sentinel。正常矩阵 callback总调用239200；guard另计。
Windows需冻结作者的真实平台实现及已接受链接 closure；当前这一步不擅自添加它。

guard最小参考范围还没有收窄为 planar `0..16`，这里只验证共用 angular
`[-1..31]` 合同。NEON ABI sentinel、各容器原生 guard 执行、benchmark和所有
shipping/HAP/live gates均在后续，不能从当前矩阵源码存在推出已通过。

## 实际 context 和 selector RED

`--selector` 为每个 `8/9/10/12/0/7/16` 先零初始化完整context，然后在同一个
实际 initializer 下分别强制CPU off(0)/on(`AV_CPU_FLAG_NEON`)。所有字段非NULL；
全部 nontarget slots 保持相同，未知位深 off/on 都必须和同TU的CPU-off8表一致。
纯原版模式还要求8位两个目标槽 off/on完全相同。

`FRD_HEVC_SELECTOR_RED` 只定义两个 never-called marker 的精确未来名称
`ff_hevc_pred_planar_16_8_neon` / `ff_hevc_pred_angular_16_8_neon`，只供取地址。
只允许 `--selector`；RED binary从不执行像素路径。实际原版 init off/on仍选同一
C callbacks、所有其余context断言成功、两个on目标槽都不等于marker，才打印：

```text
RED selector: actual ff_hevc_pred_init depth=8 CPU=NEON keeps C; planar16/angular16 target symbols absent from both slots
```

并以 **3** 退出。其他失败退出1，用法失败2；编译/链接失败、signal/crash、调用marker、
部分目标变化、其他context变化都不是RED。源码尚未执行，此处只是精确预期失败。

后继GREEN `FRD_HEVC_REQUIRE_NEON` 不定义marker，必须链接真实assembly symbols，
检查实际 on目标槽精确等于真实符号、off不等于它们，nontarget不变；数值组调用这个
实际context。两个宏同时定义直接编译报错。纯 C/model mode完全没有未来符号引用。

## 未执行命令模板与依赖门

下列命令**没有执行**。root须先解除freeze，绑定源/header/config/现存objects/
libavutil/hash、编译器和flags，并批准独立OUT新目录。不运行configure/make，
不写既有接受tree，不覆盖已接受对象，也不链接整个libavcodec来混淆实际init来源。

MacARM64配置实际存在，完整类型定义可读但编译尚未确认。需要root确认 `pred.o`
未解析symbol closure，以及实际dist `libavutil.60.26.102.dylib` 的install_name/依赖/runtime装载。
静态读源显示cpu对象还涉及 `av_log` / `av_default_item_name` / `av_opt_eval_flags`；
仅 `cpu.o+aarch64/cpu.o` 不能声称闭合。两棵树的 `libavutil.a` 均不存在。

```sh
FF=/Users/sun/FreeRemoteAccess/target/task4c5c1-runtime/ffmpeg/build.V9P45r/source/ffmpeg-8.1.2
AVUTIL=/Users/sun/FreeRemoteAccess/target/task4c5c1-runtime/ffmpeg/build.V9P45r/dist/lib/libavutil.60.26.102.dylib
TEST=/Users/sun/FreeRemoteAccess/third_party/ffmpeg/8.1.2/tests/hevc-intra16-reference.c
OUT=/Users/sun/FreeRemoteAccess/target/task4c5d1a-reference-approved-new
mkdir "$OUT"
/usr/bin/clang -std=c17 -O3 -fPIC -pthread -D_ISOC11_SOURCE -D_FILE_OFFSET_BITS=64 -D_LARGEFILE_SOURCE -DPIC -I"$FF" -I"$FF/libavcodec" -I"$FF/compat/stdbit" -I"$FF/compat/dispatch_semaphore" -DFRD_HEVC_ORIGINAL_TU "-DFRD_HEVC_ORIGINAL_PRED=\"$FF/libavcodec/hevc/pred.c\"" -c "$TEST" -o "$OUT/original-pred.o"
/usr/bin/clang -std=c17 -O3 -fPIC -pthread -I"$FF" -I"$FF/libavcodec" -I"$FF/compat/stdbit" -c "$TEST" -o "$OUT/reference.o"
/usr/bin/clang "$OUT/reference.o" "$OUT/original-pred.o" "$FF/libavcodec/hevc/pred.o" "$AVUTIL" -o "$OUT/reference"
DYLD_LIBRARY_PATH="${AVUTIL%/*}" "$OUT/reference" --reference
/usr/bin/clang -std=c17 -O3 -fPIC -pthread -I"$FF" -I"$FF/libavcodec" -I"$FF/compat/stdbit" -DFRD_HEVC_SELECTOR_RED -c "$TEST" -o "$OUT/selector-red.o"
/usr/bin/clang "$OUT/selector-red.o" "$OUT/original-pred.o" "$FF/libavcodec/hevc/pred.o" "$AVUTIL" -o "$OUT/selector-red"
DYLD_LIBRARY_PATH="${AVUTIL%/*}" "$OUT/selector-red" --selector
```

root必须捕获前一程序真实exit0及119600 cases，再单独捕获后一程序完整stderr和
exit3，不能将shell的笼统nonzero认作RED。flags是可审的拟用私有flags，不声称与
shipping全flags相同；性能门必须用shipping一致flags另做。命令只声明执行方式，
不是编译或装载已通过的声明。

额外guard程序在正常harness TU的编译行追加 `-DFRD_HEVC_GUARDS`，生成另一个
新对象/程序；不得给ORIGINAL_TU加入平台实现。Windows需另带真实platform对象，
由root审查后才固定模板；当前标记 `NEEDS_CONTEXT(platform execution/link closure)`。
GREEN命令需要后续真实actual-pred+init+assembly对象，不能现在拿marker作后备。
OHOS、Linux ARM64、Windows ARM64、disable-neon、各容器ABI/guards仍待各自门；
macOS仅ARM64，没有新增macOS Intel支持。
