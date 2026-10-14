# 私有 HEVC16 NEON D1 源码检查点

2026-10-14：**SOURCE-READY / STATIC ONLY，零 compiler/link/test/ABI/bench/profiler 执行**。
本测试、ABI sentinel、私有 postimage 和命令不进产品库、recipe 或 HAP。Root 必须先独立
审查源码并显式 release correctness；benchmark 另需独占 release。SOURCE-READY 不等于
数值通过、NEON 实际分派或全部 ARM64 平台资格。

生产补丁 `../patches/0002-aarch64-hevc-intra16-neon.patch` 只改变五个路径：新增
`libavcodec/aarch64/hevcpred_neon.S`、`hevcpred_init_aarch64.c`，及最小接线
`hevc/pred.c`、`hevc/pred.h`、`aarch64/Makefile`。原 `pred_template.c` 每 byte 不变。
固定 FFmpeg 8.1.2 归档 SHA256 为
`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`；本轮未重读归档。
现有 H264 patch 路径与本补丁完全无交集，可在 H264 patch 后精确应用；没有改旧 recipe。
所有新核与真实 initializer 的 hash、五路径 pre/post 和依赖 pin 见 NEW 私有输出
`target/task4c5d1b-source-static/source-static.json` 及 binding report。

仅 `ARCH_AARCH64 && HAVE_NEON_EXTERNAL && CONFIG_HEVC_DECODER`，位深严格等于8，
运行 `have_neon(av_get_cpu_flags())` 才选择两个真实 symbols，槽是 planar/angular `[2]`。
4/8/32、DC、intra_pred、9/10/12 与未知位深原表保持。NEON-OBJS 同时管 init 与 assembly，
调用点又有相同编译门；disable-neon/decoder-disabled 实际 compile/link 仍待 release 后独立执行。
CPUoff保留C只作oracle/unsupported-CPU路径，不能作为新增生产 scalar 资格。

Windows 当前私有 prototype 显式拒绝新 slots；两函数不输出 Windows 定义，caller不引用，
私有 native wrapper/ABI 文件用 `#error` 拒绝未闭合的 Windows adapter。现有 Windows 默认行为
没有被本任务替换。Windows ARM64 的两个槽与 SEH、COFF symbol、原生 numerical/guard/ABI
是**必须完成的 OPEN 后继门**，不是永久产品不支持。组合 recipe、广告和 Windows 新核选择
必须拒绝这个候选；不能把保留C当作静默生产 fallback。Darwin/ELF 源码亦尚未编译/执行。

Planar仅读 top/left[0..16]，各组8个u16 lane；最大分子8176（含唯一rounding16），用
`rshrn #5` 一次舍入。Angular只用基线NEON，表复现原值，signed `asr` 算floor idx与
inverse-index；`last=-1` 不扩展，`last<-1` 的scratch只复制17个必要bytes而不增加padding。
fact0仅加载一个16byte向量，fraction加载两相邻16byte向量且一遍u16乘加/rounded shift。
水平路径完整列预测后用原版 `transpose_8x8B` 四次转置；mode10/26 luma以signed h lanes、
`sshr #1`、`sqxtun` 做负奇差及clip，vertical只把已算好的byte lanes散写第一列。
没有scalar逐pixel插值或边界数学。参考合法共同范围[-1..31]，无额外padding/对齐假设，
top==left允许，dst与预制refs无重叠；signed stride直接用于所有dst行地址。

Planar leaf无stack，使用x0..x9、v0..v7/v16..v23。Angular leaf使用x0..x17，v0..v7/v16..17，
不写x18或x19..29/d8..15，不改LR；SP减320并保持16对齐，scratch[0..63]，ref=SP+16，
实际初始化ref[-16..16]，水平tile[64..319]。dst没有额外行/尾写入。使用原版asm.S的
`function/endfunc`、`movrel`、BTI入口；leaf无需保存LR，angular含CFI的SP offset记录。
Windows stack/unwind未来必须单独补真实SEH与原生门，不能由CFI/ELF/Darwin结论推导。

新wrapper以 renamed-main直接include冻结 `hevc-intra16-reference.c`，固定
`FRD_HEVC_REQUIRE_NEON` + `FRD_HEVC_GUARDS`。没有复制、缩减、改写其model或actual original
include-TU；运行 `--native` 先执行真实context exact GREEN、119600 regular cases，再执行
原4800 angular共同范围guards，然后才执行新增门。RED仍来自冻结旧原版actual对象的独立程序；
不能把这个wrapper的missing-symbol/link failure当RED。GREEN须选择两真实symbols，直接call不替代。

新增有限inventory如下，全部尚未执行：

- planar最小 RO refs[0..16]，前/后guard端点各贴页，46图案×8strides×两侧×top==left/独立边，
  **1472 cases**。短页上只从完整pattern缓冲拷贝17bytes，再调用冻结model与真实selected核；
  不在短refs上运行angular或原版pattern生成器。全dst页、padding/redzones及两refs页逐byte比较。
- direct ABI：46图案×8strides×shared/独立×100planar/angular cases，**73600 cases**；
  dst/top/left residues固定7/1/3，与原矩阵独立全residue覆盖配合。整个4096byte输出、
  两96byte输入比较，合法negative stride保留。helper直接BL数值symbols后，先比较LR/全部
  x19..29、d8..15 low64、x18只读snapshot、SP，最后才恢复caller状态；没有restoring C bridge。
  GPR/SIMD sentinel均是完整非零high64-pattern，x2仍完整referencepointer，x4/x5 high32刻意污染，
  kernel只读取w4/w5的defined int值。mask bits0..10=x19..29，11..18=d8..15，19=x18，20=SP，21=LR。
  SP/LR损坏造成signal或无法返回同样FAIL；正常返回mask0本身仍必须和真实像素一致。

可审的执行准备位于 NEW
`target/task4c5d1b-source-static/commands-correctness.sh`。它尚未执行，默认exit2，只有root显式
release并绑定never-existing output才允许执行。只编译未改original include-TU、新pred/init/ASM、
新nativewrapper/ABI，链接现有已绑定Mac dist avutil，不configure/make，不写接受tree或建第二FFmpeg。
Apple `/usr/bin/clang`、private `-std=c17 -O3 -fPIC -pthread` 等完整flags在script，不是shipping
一致flags声明。私有partial-postimage的actualpred编译单独 `-include` 新postimage/pred.h，
再用真实原版hevcdec.h/template依赖；否则先进入原hevcdec.h的旧pred.h guard会隐藏新init声明。
这是实际修改header的include顺序绑定，不是fake类型或改oracle；originalTU不使用该flag。
root需记录真实argv/exit/stdout/stderr、before/after源/object/lib/compiler identity、
实际dyld loaded avutil。OHOS/Linux需要各自真实compiler/config/lib closure与platformflags；Windows
本私有形态明确不可用。macOS仅ARM64。

Correctness release 后的独立反汇编必须覆盖 linked两个symbols及表，确认数学NEON乘加/舍入、
水平transpose、signed boundary correction、无x18/nonvolatile写入、stack320及真实代码extent/hash。
compile-only的disable-neon/decoder-disabled门需绑定真实匹配config，实际编译与链接验证没有新symbol
未解析引用；仅用 `-D` 被config.h覆盖的假禁用不算门。nontarget/higher/unknown需用原wrapper完整context。
原source/template及旧H264/hash保留由source-static记录，不拿未调用的高位深pixel资格填绿。

另立的有意义负控应在NEW私有mutation目录复制**仅新postimage**，保持原model/TU不变：错误槽[1]
由exactselector拒绝；`rshrn`改`shrn`由pattern28/29 rounding witness拒绝；signed idx `asr`改`lsr`
由mode12/17/19/24负角拒绝（signal也是FAIL，不算RED）；跳过8×8transpose由mode2/12非对称梯度拒绝；
`sshr`改`ushr`由mode10/26 c=0 pattern24/26/27负奇差拒绝；写17bytes由stride17/31完整padding检查
及minimumguard拒绝。需实际运行并记录各witness/exit，不能拿source中存在这些测试当负控通过。
原angularguard允许C四byte copy至[18]，不会把其合法读取错误地列为overread。

固定benchmark源码受单独 `FRD_HEVC_BENCH_RELEASE` 编译门控制，默认binary不支持 `--bench`。
Root correctness PASS后需**另行独占release**，绑定与待接受shipping对象相同compiler/flags、实际
object/lib identities、关闭profiler并暂停并行构建；把nativewrapper用上述额外宏编译后链接那些对象。
执行准备 `commands-benchmark.sh` 要求独立release marker与单独boundbinary，默认不会运行。
私有prototype就算timing通过，也不是最终shippingperformance；recipe/package/target阶段仍要重绑对象。

固定seed0x4c5d2026，沿冻结46图案输入池，每C/native kernel65536warm，31batches×262144calls，
每batch/run交替C/native先后；每组3完整runs，不挑最好。planar独组，angular modes
2/10/11/12/17/18/19/25/26/27/33/34分别×luma/chroma1（25groups/run）。计时外逐输入池比较及每batch
末输出/wholepadding比较并checksum；printf/排序/checksum在计时外。输出calls、median(index15)、
nearest-rankp95(index29)、max(index30)、比值/所有misses。数学组每轮median<1.0×C；其余组
median<=1.05×C且p95<=1.10×C。math分类为planar、非0/±32 fractional角及mode10/26 luma；copy和
mode10/26chroma为other，max全部披露。任何miss退出1，不减batch、不换输入、不事后改阈值。
未锁核/频率、clock与调度环境须真实披露；最多一次有记录环境故障重测仍由root判断。

D2、multi-recipe、OHOS phone/PC普通HAP、Mac/Linux/Windows包、native runtime、vector PC、
HEVC完整wire/profile/backend和live互操作全部独立后继门。本文件与源码存在不注册新codec，
不改NativeFirst、不改变默认或旧H264资格。

## Mach-O label fix round 1 与私有禁用配置准备

Root审查首次真实compiler失败和最小diagnostics后，只授权本轮将新productionASM/ABI的
`.L...`私有labels/references换成`L...`。Mach-O临时label/atom规则导致原CFIadvance差无效；
最小`.Lbody`probe真实compile exit1，而`Lbody`exit0。此次修复不改变任何数学、selector、
flags、测试矩阵或接受树；production32处、ABI8处前缀变化。当前delta已冻结，**尚未重跑compiler/
correctness，需root独立delta review后新release**；初始SOURCE零执行计数是历史，首次release
失败细节与对象身份见bindingreport和`target/task4c5d1b-correctness-q5ndwhhi/results.json`。

Root接受完整privateheader方案仅作为PRIVATE compile/link门。准备位于NEW
`target/task4c5d1b-label-fix-round1`：`configs/disable-neon`和`configs/decoder-disabled`均含
真实Mac配置的完整config.h/config_components.h副本；前者只将HAVE_NEON/HAVE_NEON_EXTERNAL/
HAVE_NEON_INLINE置0，后者只将CONFIG_HEVC_DECODER置0。其余bytes逐一保留，完整headerguard有效。
`config-identities.json`保存base/copyhash及精确修改项。没有运行configure；原FF配置字符串仍
保留来源身份，不能将这些副本冒称configure输出、Makefile对象选择或shipping包资格。

`commands-disabled-gates.sh`默认exit2，等待delta review与独立correctness release。它先真实
`clang -E -dM`消费这些完整privateheaders，`config-assertions.c`用#error验证实际NEON/decoder bits
及ARCH_AARCH64；仅FRD_EXPECT宏选择断言类型，不用-D伪造HAVE/CONFIG值。随后forced-include完整
privateheaders并include顺序绑定实际新pred.h，真实编译actualpred/init、未改originalTU与冻结原版
reference guard形态；link故意不提供任何newASM/marker，然后运行真实原context/C/model119600+
4800guards，nm检查两C对象没有newkernels未解析引用。每child真实argv/exit/PID/session/loadedavutil
和完整before/after依赖须另记，script本身不是执行证据。

接受source的configure:4487拒绝source已存在config.h的out-of-tree configure；本任务不删除它，
不复制或构建第二完整FFmpeg。此私有位配置门不证明完整--disable-neon configure、其他对象依赖、
正式recipe/package、Windows/native或任何shipping资格。Benchmark继续HOLD。
