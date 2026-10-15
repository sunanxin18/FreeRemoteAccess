# Apple MVS ARM64 effective-DC 候选验证

2026-10-16。实现状态为**实验性**；仅源码、宿主正确性与最终 OHOS ELF 静态 ISA 门通过。默认 `mvs-native-idct-arm64` / `mvs-native-arm64` 仍关闭。当前候选没有手机/PC执行或性能结果，不继承上一 [CSD 候选](mvs-idct-csd-arm64-20261016.md) 的设备资格；旧 CSD 的 MIXED 结果和 adverse 全部保留。

## 改动与数学边界

基于 `00b328cd51a96d2a47a54301a14dc99773a811ba`，唯一产品文件为 `crates/frd-protocol-apple/src/mvs_idct_arm64.rs`。每个自然序 AC index 1..63 必须满足系数为零或量化为零，才进入整块 DC 早返。八组 NEON 比较覆盖完整输入，仅排除第0项；判定不依赖最终像素同色、原整数位 AND、低 byte 或截断乘积。

设原反量化 DC 为 `D=i64(c[0])*i64(q[0])`，原两遍得到 `Wdc=4D`、`S=floor((D+4)/8)`。负半数向上舍入：-4→0、-5→-1、-12→-1。D 全域为 [-8355840,8355585]，保留精确 signed i64，再调用原 Apple 10位环绕表。四个有界16字节写入生成完整64字节输出。非DC块继续原完整CSD蝶形；dispatch、原scalar oracle、颜色、wire/cache/transaction和measurement fixture均未改。

首个 intrinsics 产物经过 LLVM 优化，消去了最终10位映射之前的 signed/full-i64/SSHR 形式，实际成为零扩展、UMLAL/USHR。它通过宿主像素测试，并非已发现像素错误，但不满足预先要求的实际 ISA 契约；失败候选和日志保留。最初 ISA 脚本拒绝的直接原因另为过严的全BL禁令，不能称脚本自动发现 narrowing。root 检查真实机器字后仅放行四条向量 asm，固定 SMULL/DUP/ADD/SSHR #3；原 transform 的两个 outline 叶在完整调用闭包核对后允许。

## 相同基线、构建与宿主验证

先把共享测试加到原生产源码，冻结 N0，再真实构建并完成其资格，随后才实现 N1。N0/N1共享测试字节完全相同。原生产源码 SHA256 `fc7d07d564d1f731829ab3f6421f6b167c7f2908dc2dc05f9e5bb14c615cd6ce`；N0 `52e9cf08df51d25a25d924c6ff9c4a3f1f2bf322350de3b580d43ee123d734a4`；最终 N1 `056c8b8327cf0ed549cf83bcdf2b1f2b5359788849f2ee1dce1a236f0b0cc13d`。

使用固定绝对 Rust1.96.0、官方OHOS API26 SDK，offline/locked Release libtest，两版每个平台的argv/env、依赖/features/profile一致。实际Cargo JSON的Apple executable均fresh=false，并分别复制；141个非Apple依赖工件每个平台逐项相等。实际dep-info并集为65本地/694 registry编译输入，本地均包含于79份固定项目输入；694是host+OHOS并集，不能称host单独数量。60个registry包的2657文件、锁文件checksum、FDK、工具链及124系统库身份已冻结核对。

| 复制的实际 Release libtest | SHA256 |
|---|---|
| N0 macOS ARM64 | `80b699b9369fb8d07168f1103f3087e6bd78d9af7ea58c502534b5ec4e90598f` |
| N0 OHOS ARM64 | `c5df4dee59015ac9f0610d43392ce27479a525881b0e12180bf5a1f19924483e` |
| N1 macOS ARM64 | `40b6a0babbad056eed2c671ffa68b1cd729e309d3092552248b9e05ae43ae881` |
| N1 OHOS ARM64 | `0192cb8a5a10fc686489e731d73799887dd217073547931832e6a914c0282b54` |

共享全域测试枚举16777216个i16×u8组合，以独立i128/div_euclid核对真实SIMD D及原两遍全部八lane的W/S，发生在表映射之前。候选另对同一全域直接核对实际新helper；完整入口对照覆盖七个表周期、负数/极值/量化>=128、两种有效零、全部63个AC位置及假零反例。六个测试内错误模型被精确相等断言拒绝；这是预期panic控制，不是生产代码曾失败的声明。

实际命令结果：N0/N1各host与OHOS no-run构建exit0；N0共享3项及旧CSD直接1项通过；最终N1聚焦17项通过；完整Apple复制libtest为560通过/11忽略/0失败/0计时；package fmt check exit0。既有runtime dead_code警告及`--nocapture`下预期负控panic输出保留，不能称无警告。构建/测试均未由审查者重跑。

N0/N1各自新目录运行一次完整wire资格，32条record、8个state和DONE。N0↔N1、旧default↔N0、旧default↔N1三组比较各386897350字节全部相等；比较完整framebuffer、patch/stride/geometry、Damage/Boundary顺序、cache/state及已提交framebuffer对应区域，并拒绝pixel/rect/order/state四类负控。不是hash-only资格。首次比较因输出父目录缺失而失败，同一未改比较器创建目录后通过，原失败保留。

## 最终 ELF 的独立静态核对

最终N0/N1入口分别257/351条指令、1028/1404字节；均1072字节栈、两个BL。共享测试改变了本轮N0的outline形态，不能把旧CSD报告542条当本轮N0。两个原transform叶各164条/656字节，N0/N1机器字完全相同，无分支/BL/栈访问/GPR乘法；每叶保留八个i64 SSHR #11或#18。full/mode5、type1消费者各有三次真实native调用。

N1检测完全SIMD展开，八组load覆盖coeff字节0..127、quant字节0..63。布尔mask窄化保持语义，只有DC lane被清除。实际有符号扩展、正数quant输入、关键机器字 `0ea1c004/4e080484/4ee28484/4f7d0484` 与rodata两个i64 4绑定；原环绕表后byte广播、两个STP精确写完64字节。没有新增隐藏热调用。早返仍分配原栈，不能声称消除栈成本或已测提速。

独立审查 Spec PASS / Quality Approved，无Critical/Important。root独立核对全部634份材料、79份当前项目输入与Git基线（仅IDCT变化），并从两个真实ELF的PT_LOAD映射逐字核对入口及328个leaf机器字、直接BL目标和常量。源码报告SHA256 `69f6117123121d4e4f617366b43c5923daa2f43cec640e2b35aa21db820e571f`；634项manifest `87e31ba3ca2549c58951fa444b5d1d44ca3caf2de3da0e532ba573e67608a6e4`；独立审查 `bca1a0e920ff4d201315cdf73fb5dcb13f3debbcb2fcef35e569219bf082ae64`。完整证据在 `target/mvs-dc-implementation-20261016/`，root接收在 `target/mvs-dc-preflight-root-20261016/implementation-*-acceptance.json`。

## 后续未完成的门

先对同两版OHOS产物单独完成目标直接正确性及完整32+8资格、host↔target全值比较，再单独释放原八工作集固定ABBA及全部warmup、3samples、4records、apply/publish/total和aggregate门。六个no-IDCT控制、原adverse和共享宿主负载限制必须保留，不追加样本或改用record分组直到变好。

本轮没有设备命令、PC观察/操作或手机执行，没有measurement、性能晋升、HAP/FFI/GPU、网络、真实Mac会话或其它架构资格。macOS仍仅ARM64；Windows/Linux及既有功能选择未改。完整产品计划、PC人工填写hold和生产默认边界保持。
