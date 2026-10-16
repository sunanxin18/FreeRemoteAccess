# RDP RGBA→BGRX 的 SSE2 转换

2026-10-16，状态：**开发中**。本次补齐 Windows/Linux x86 与 x86_64 的 SSE2 转换主体；Linux/Windows i686与x86_64已有新体局部实际运行；Windows保护页、各目标ABI和性能尚未完成，不能据此授予完整解码器或客户端生产资格。

RequiredNative 的 x86 准入要求 SSE2，原像素发布转换却只在 SSSE3 可用时使用原生实现，否则回到标量。现在保留 SSSE3 优先，其后检查真实 SSE2，再选择共享的 SSE2 入口；只有未通过 SSE2 检查的 CPU 才使用明确的 reference 后备。协议、解码器选择、Surface 合同、默认策略、能力广告和 ARM64 实现保持。

每个32位小端 lane 以移位、掩码和 OR 完成 `[R,G,B,A] → [B,G,R,255]`。完整16字节块和最后1–3个像素使用相同向量核心；尾部只搬运实际4/8/12字节，不读取调用方 padding。输入长度和目标容量在写入前检查，成功时也只修改目标对应前缀。

测试使用独立字节 oracle、literal RGB/opaque-X 锚点、40种像素数和16×16种源/目标偏移，共10,240例矩阵，并检查非法输入不写入和目标后缀保持。x86 测试只能否决真实 SSSE3，不能制造 SSE2 能力；实际 SSE2 入口的测试计数用于区分像素相同的标量后备。该计数仅在测试构建中存在，不能用它推断生产性能。

| 检查 | 当前结果与范围 |
|---|---|
| 固定 Rust 1.96 格式 | 两源通过 |
| macOS ARM64 局部真实源码测试 | debug/release各8通过；各过滤1项既有benchmark。仅共享API及NEON回归，不是x86执行 |
| Windows i686/x86_64 | 两个实际源码优化COFF对象编译通过；后续普通CI已执行Windows x86_64新体12项、i686新体11项局部测试 |
| Linux i686/x86_64 | 两个实际源码优化ELF对象编译通过；后续普通CI已执行x86_64新体12项、i686新体11项局部测试 |
| SSE2静态指令 | 四个精确入口及内联数值核心仅见SSE2指令；外部memcpy/TLS/panic运行库未链接，不代表最终ISA闭包 |
| 原体反例/新体执行 | Linux x64/i686冻结原体实际命中唯一counter反例；Linux/Windows x86_64新体各12项、两平台i686新体各11项实际通过 |
| Linux私有non-test保护页 | x86_64/i686实际普通release ELF，各kernel各160合法case及五精确SIGSEGV负控通过；不是shipping库资格 |
| Windows保护页、ABI/unwind、有限性能 | 尚未完成；sentinel和对象编译不能替代 |

最终源SHA256：`pixel_convert.rs` 为`ee5623097b839166f311ffc0a9fffbfbc3d8b59163da96df8b9171692f4fa66c`，`pixel_convert/x86.rs` 为`232fabaca6781a9e3ef5110de292d3c4b017066b847f7ee9b31ad4f7ad8aeaf9`。原SSSE3数值体与ARM64文件保持；macOS只支持ARM64，本次没有macOS Intel构建。

报告和原始记录在`target/sse2-publication-implementation-20261016/`，root保留原两源在`target/sse2-publication-root-20261016/before/`。独立源码审查与后继真实运行结果分别记录，不把源码实现、对象编译、实际执行和性能混为一个通过状态。

独立源码审查结论为Spec/Quality PASS，未发现需返工问题；报告SHA256为`c8bf96bd718b7f5057862e4b0c36e0c9e094a21e449d8318daef58c867c9b66b`。root已完整读取两源diff、实现报告、审查报告，重新核对源快照及四目标对象身份。此结论仅接受源码切片，表中真实目标运行门继续开放。

Windows、Linux包流程已分别补上仅i686执行的实际crate局部测试接线，使用原目标/linker/runtime，跳过既有benchmark，失败即停止。两处独立配置审查通过（报告SHA256 `4208413f73f020396eed668867246d67a611e430403c71452d214b6e06cef6bc`）；两平台i686后继均已取得下述实际执行结果。x64沿用现有完整crate测试，不新增重复运行。过滤器须实际运行11项局部测试（含三个ISA测试），不能用0测试的成功退出当验证。

## 首次 Linux x86_64 真实运行

提交`ba6035752f746651681708c07cefa1c896248880`的[普通CI38847485935](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38847485935)中，Ubuntu24.04 job116801271211成功；2026-10-16 03:54:47 UTC原日志记录该模块12项实际测试均为ok，包含三个ISA测试和既有benchmark。关键`publication_actual_sse2_dispatch_when_ssse3_denied`、直接SSE2/否决SSSE3矩阵和SSSE3优先恢复均实际执行通过，不是对象编译或空过滤。完整原job日志在`target/sse2-publication-root-20261016/ci/ba603575-linux-x64-job-original.log`。

这将Linux x86_64的新体局部正确性从“仅编译”推进到实际GREEN。旧体实际RED、i686对应运行、保护页、ABI/unwind、最终链接运行库ISA闭包及三路性能仍待完成；旧benchmark显示ok不等于本次新增SSE2路径已有速度收益。该CI的Mac ARM64及格式任务同样成功，不能外推其他架构或真实远程会话。

独立执行读数复核报告SHA256为`bbe881fe8f14c3e209d5d956c8e577b99468130ec69abce25e49db6d962aac84`，核对原API的head/job/runner、原日志中的1.96工具链/x86_64 host和逐名12条ok；不追加CPUID硬件清单或速度结论。

## 首次 Windows x86_64 真实运行

同提交`ba603575`的[Windows普通包run38847485965](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38847485965)中，已完成的x86打包job116801202209记录无`--target`的宿主Rust测试，实际host为Windows x86_64；2026-10-16 04:00:36–37 UTC的12条pixel_convert测试均ok，包含三个ISA测试。这是x64宿主执行，不能按矩阵名称记为i686执行。完整原log为`target/sse2-publication-root-20261016/ci/ba603575-windows-x64-host-from-x86-job-original.log`，465,453字节，SHA256 `618c209981a2f405c5a6a4b538eaee917692a8ebb0d460cc328201b549858a29`。

该证据推进Windows x64新体局部正确性；i686定向接线的实际结果、旧体RED、保护页、ABI、最终运行库ISA及三路性能继续开放。原benchmark通过不作为本次SSE2速度结论。

Windows x64独立执行读数复核报告SHA256为`0e1795072152e000428ee734c27e3d446ca56405e2593e35b1f5fadb79e622fd`，确认匹配head、x64 host及12项实际ok，不按x86矩阵名称外推i686。

## 首次 Linux i686 真实运行

提交`dd46ea849f86cee3f5bcc074b41cc05a219c4f50`的[Linux包run38849395853](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38849395853) / job116807020652实际通过11项局部测试，0失败、0忽略，包含三个ISA测试。命令显式指定`--target i686-unknown-linux-gnu`，实际启动i686目录内测试程序，既有benchmark明确跳过。日志中deny guard检查的panic由`catch_unwind`捕获，同一测试最终ok；它不是本次失败，也不能替代完整ABI/unwind验收。

原日志`target/sse2-publication-root-20261016/ci/dd46ea84-linux-i686-job-original.log`为405,242字节，SHA256 `b090e6bca631563d6b2a480d329d47f79eec0c9ac5fd397ac0d8f08223dcb989`。独立读数复核报告SHA256 `c235ed78a3baf28d5a505202577c99301151db25eed1d8239219d7eddede8b60`确认匹配提交、真实目标进程及逐名结果，root已完整读取。Windows i686、旧体实际RED、保护页、ABI、最终运行库ISA闭包和性能仍待完成。

## 首次 Windows i686 真实运行

提交`263d7bf04bea7f1c13a36154cb053cf4e372c1e7`的[Windows包run38851041834](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38851041834) / job116812401497已成功。原日志4826–4860段显式指定`--target i686-pc-windows-msvc`，运行对应target目录内exe，实际11 passed、0 failed、0 ignored，含三个ISA控制；benchmark按命令跳过。预期`catch_unwind`的panic随后同项ok，独立复核确认不是失败。前面宿主12项与本段i68611项分开，不拼接性能结果。

原日志`target/sse2-publication-root-20261016/ci/263d7bf-windows-i686-job-original.log`为469,099字节，SHA256 `b83eea111c602a154e61ee5ede35e4e6ea2fbaaa96638b0a131c9e9d25d6cc0b`。独立报告SHA256 `4f88162fe2505be1f27288f25c30f6851f5369612109f472b59b21a353aeab9a`，root已全文读取并核冻结。两平台x64/i686的新体局部运行已齐，但旧体实际RED、保护页、直接ABI、最终shipping ISA/运行库闭包及有限性能仍待完成；模块GREEN不等于完整解码器或远程会话资格。

## Linux 保护页入口准备

新增K crate私有普通example，直接编译实际RDP x86叶子，避免`cfg(test)`计数进入测量/内存检查；不复制转换核。它在Linux x86_64/i686上显式检查ISA，分别运行每可用kernel的10,240例独立字节oracle，以及160组输入/输出上下边界与只读源页检查；五个隔离故障负控制必须在ready之后准确SIGSEGV。其它架构/系统明确退出77，不以零测试计成功。

初审发现观察错误会跳过child清理，现已统一进入保留原错误的kill与有界try_wait，无法确认时明确unconfirmed。修复经独立源码复审通过，报告SHA256 `72a3e3776528df266fea83a39127a115cb102d2be1a32b676e928c9960a08593`。Mac ARM64仅执行拒绝门与实际owned child清理控制，两个Linux目标仅完成非test metadata检查；实际OS观察错误与清理失败未运行覆盖。保护页原生运行尚未开始，ABI、性能和shipping ISA继续开放。

后继沿用现有Linux x64/i686矩阵，普通release build后顺序运行parity/guards，失败即停并保存固定原件；ARM64跳过该专用步骤。此接线不改变产品decoder、协议、manifest或默认行为。

现有Linux矩阵接线经独立配置审查，identity写入失败时保留原非零状态的P3也已关闭，最终报告SHA256 `70dd0111c9c319cd5af435872a129170aa870c51f0606148b600ed3d97435023`。root已读完整两源、清理delta、workflow完整增量及最后分支，核当前源身份后，只释放随审定提交push的首次Linux x64/i686实际parity/guards；不另dispatch或自动重跑，运行结果另记。

## Linux x64/i686 实际保护页通过

提交`59cdd705eaeebbe7c0a69f5f43c1b76ecc672240`的[Linux run38854683582](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38854683582)中，x64 job116823393522和i686 job116823393262的专用native及上传步骤均已通过。两端各以普通非test release bin运行实际叶子：SSE2/SSSE3各10,240例parity、各160例合法保护页case，valid child exit0；五类故意访问分别在ready后准确SIGSEGV，最终GUARDS_OK。采集时整个包job仍在运行，不能把该局部结果记为整个workflow成功。

两份原ZIP各恰8成员，包含四日志、ELF与三源；三源before/after及归档字节一致。x64为ELF64/machine62、SHA256 `03fce6dc05138d4a65ae42c5f8b0033eaf9952beabd06c1bd4f670b6f92258ce`；i686为ELF32/machine3、SHA256 `31035213178eab4cd12842a877701ecb3660c233a840b63a42078244c3b65004`。root只读核对原件和构建命令，没有执行下载ELF。独立报告SHA256分别为`2879e12ccb3c063f7cda18aeb8e993852338a707b42bffc1ced6e0aedc8113d5`、`e39c52ec5256bf0cb36325787c64e8ff5fee8cbd9ed13f8e18900e235478ac72`。

这关闭Linux两个私有普通binary的有界parity/guard运行门。旧体实际RED、Windows保护页、直接ABI、最终生产shipping ISA/系统runtime闭包和有限性能仍开放，不外推到完整解码器、包或live产品资格。原故障负控状态中的`core dumped`文本保留，不据此声称有core文件或产品bug。

## 固定旧体反例的原生执行接线

四个冻结fixture已随仓库交付，保留原SSSE3→scalar dispatch及仅否决真实SSSE3的测试hook；Linux x64/i686各编译一次，只运行具名counter测试。接受条件同时要求编译0、运行101、唯一精确counter断言在第195行失败，以及0 passed/1 failed/9 filtered；像素字面值和suffix检查在该断言之前。任意非零、CPU前置检查失败或意外GREEN均不算预期反例。十个精确原件分别保存源、exe、四通道日志及身份，不改变生产源或默认策略。

作者报告SHA256 `42c9cb27b404a917de925bfef70b0bbd88d1f39ae3363985e1403aed872e463f`，独立审查 `be63ed1efeeec81b9af12ee89d9f44013658d8059f079b0eeb6d6cc582eb13aa`。root已全文读增量及两报告，并重新核对五源和全部20项审查输入。此次只接受源码与执行接线；首次随本次功能分支push执行，实际两个目标RED仍待原始日志核验，ABI、shipping闭包和性能保持开放。

## Linux x64/i686 冻结旧体实际反例成立

提交`cd4dbc80344c6313dec9549b35d21ce7e38f8292`的[run38856515688/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38856515688)，x64 job116829088193与i686 job116829088586的反例及上传步骤均success。两端均编译0后运行指定单项并退出101；唯一失败为fixture第195行counter左0右1，像素和suffix检查已先通过，严格verdict为accepted-original-counterexample。这证实冻结旧dispatch在否决SSSE3时走标量，未将任意失败算作RED。新体实际GREEN保持前述范围。

两ZIP各恰10成员，源before/after/current、四通道日志、目标ELF及身份均已核对。x64 ELF64/machine62、exe SHA256 `66b782c2bd8fc4eec2a3529c77a2de69a54ff4a3a233ca3bab98399b757f067c`；i686 ELF32/machine3、exe SHA256 `fa8eba9fee64c66bb38c523734e7fea76c9d4e868459b2aea7db1f54b76439f4`。root未执行下载内容，独立报告SHA256 `28e73276cb3dc930998caacd01af09d66e0611803e219f15c0270e05a68cb1e9`及两文件冻结已全文读和核验。原件在`target/sse2-publication-old-red-delivery-root-20261016/ci/`。

此门只接受Linux两个目标的冻结旧体反例，不授Windows保护页、直接ABI、最终shipping ISA/runtime、性能或live资格。采集时整个job仍运行，后继包终态单独记录；上一提交59cdd705的Linux三架构包及Mac ARM64包已terminal success，同样不能外推真实会话。
