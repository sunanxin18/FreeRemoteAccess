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
| Windows私有non-test保护页 | x64/i686实际普通release PE，各核10240对照/160合法guard及五精确AV通过 |
| 直接ABI/unwind、有限性能 | 尚未完成；正确像素/保护页不能替代 |

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

## Windows x64/i686 私有入口源码准备

新增Windows专用ordinary example与support，直接path引用同一实际x86 leaf，保持Linux已验源不变。固定两ISA真实准入、独立oracle/literal、每核10240parity和160合法guard；Win32三页reserve/只commit中页、只读source、五个固定self-child真实volatile故障，ready后精确u32 `0xC0000005`。缺核、无marker、任意其它非0、捕获不全或未证明终止均拒绝。它只支持私有leaf检查，不接产品工厂或改变默认策略。

作者报告SHA256 `b5424a6c4b8aafd38198d2f0a20f5bbe06b0c7b3fe4c41f06444494a21d618d2`，独立源码审查 `39fc17c5388ddd1e586bf176cd0b68994093ef2202e1bd6351e33e4fede5f265`；root已全文读两源和报告，核82项作者输入/原件与45项独立收据。现有证据仅格式0、两Windows目标ordinary/test metadata编译0、宿主四fake owner控制（释放失败保owner的RED→GREEN）及Mac ARM64普通入口77。test harness的0 passed/1 ignored明确unavailable，不能授Windows资格。

尚无Windows链接/PE/VirtualAlloc/AV/真实生命周期、直接ABI或性能运行。后继现Windows矩阵只为显式x64/i686普通release build→同一PE parity/guards接线；配置与实际执行分别审查，不从metadata推导运行通过。既有ARM64工具与Windows/Linux产品默认不受该入口提升。

Windows现矩阵的两步接线已完成配置源审，独立报告SHA256 `3f1cb8980820e3f698d4a146b552e0cb9a37acef66b4331006cbff3900752472`；root已全文读完整delta/作者与审查报告，并核15项配置材料、17项独立输入及当前hash。仅x64/i686，明确target，原18步保持；普通release build后同PE顺序parity/guards，BaseStream保存三组原双通道，终态/EOF/收尾未知即partial失败，上传11精确原件。helper只控制自有direct Process，不授cargo后代quiescence。该配置未做PS parser或运行验证，首次随本次push实际执行，Windows保护页/ABI/perf仍待验。

## Windows x64/i686 实际普通保护页通过

提交`6b95c81e854d75ccfeb45c282112ac14f7adadc9`的[run38871211066/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38871211066)，i686 job116875482647与x64 job116875482792的native及upload步骤均success，ARM专用两步skipped。每目标三个阶段实际exit0且原双通道EOF完整：两核各10240parity/160合法guard，valid0、五个精确ready后u32 `0xC0000005`，最终runtime_complete=true。实际rustc为1.96.0目录、普通bin/link、显式目标，无--test；未补写尚无完整joblog的版本细节或声称compiler body hash。

两ZIP各11固定成员，三源/快照before-after及同PE hash全部匹配。i686 PE32/machine014c为239616B，SHA256 `04deb119c1ccb90acca01915b5bff4b055e421b11a981d68b42b896a1f80b7d9`；x64 PE32+/machine8664为274944B，SHA256 `e1c478f9205aa88d6ec74a279b654fa0f486e256054a5cd144c0f3a5af1ccdde`。这是X64宿主上的实际32位PE入口，不能外推32位Windows OS全部资格。

root核原件并全文读独立报告SHA256 `e96d4e73395c17ee564f9f03e86d0404b8d007336b10a56d441cc1c0656f859c`及五输入身份；原件在`target/windows-apt-and-publication-integration-root-20261016/ci/`。采集时完整package仍运行，direct Process之外后代停止未证明；timeout/OS错误分支、直接ABI、shipping ISA/runtime、性能与live继续开放。由此四x86目标私有parity/guard门已齐，Task4A2.10C整体仍未关闭。

### 私有 direct ABI 源码与四目标静态编译

新增`rdp_publication_abi.rs`及独立x64/i686汇编helper，只验证私有address-taken正常return实例。精确Rust CopyFn使两个真实leaf地址逃逸，正向由asm直接调用真实leaf并在回Rust前检查和恢复；固定逐寄存器/高半部、SP、DF、MXCSR控制及x87控制字负控分别校准精确mask，源码闭集为Linux x64每核16、Windows x64每核40、i686每核12，两核各9个正常pixel counts。当前只是实现，全部实际校准与正常return运行尚未进行。

独立SOURCE/FRAME审查通过（`be81c7628c6fda0cd67a11c87564b4b1afab80ce330680e68d4bbde2f7f9de51`）；已更正文档中Win32无条件16字节对齐的假设，代码不改。源码与真实leaf四项在静态门前后保持，四目标各唯一一次固定Rust1.96 non-test bin `--emit=asm,obj -Copt-level=3 -Ccodegen-units=1`均0，只有unused_unsafe/unused_mut警告。原件为`target/publication-direct-abi-static-20261016/`，报告SHA256 `549644dd40394dc7ffab3eae225107bc71aaf0371785f044fdd32b35e8edb16a`、receipt SHA256 `0c5527f0c075ec770401974e5aecc0943b97af4548b98efb7ad6aca0805c0285`，root核53项绑定。

四个对象的typed caller→asm entry→target slot→两真实leaf参数链已有静态读数；Linux i686以平衡CALL/RET thunk和GOT建立独立SP锚，Win32使用PE relocation；实际RET8编码与固定scratch落点核对。Win64 reader解出Version1/ALLOC_LARGE408/8条GPR及10条XMM保存，Linux两个eh_frame读数与保存偏移一致；Win32 reader虽0却明确unsupported I386，未授展开。可读元数据不等于异常穿越或full unwind。

最终release实际使用thin LTO，当前未链接对象不能替代同一个最终ELF/PE的地址链与frame证明。下一步仅准备四目标build/link-only、同次map和精确原件运输，配置审查与实际CI另记；不运行abi。控制流保护继续not-qualified，具体最终图像/环境的外部准入、实际精确负控和正常状态门、shipping内部fastcall/inline、性能与live均开放。Task4A2.10C继续未勾选，产品默认与codec广告保持。

四目标静态原件已独立复核通过（报告SHA256 `39957ab9a342d9cefd1ca40cd13e96d131b7df70329d1c5753978eeb797af392`），root全文读报告并核38项输入。复核仅关闭当前对象的窄静态读数，不授最终链接、实际ABI、保护或性能资格。

### 最终链接材料的 CI 接线

Linux/Windows现workflow各新增x86两行专用build-only及always精确九原件上传，ARM跳过，原全部步骤/矩阵/权限/触发保持。固定一次`cargo rustc --release`保留thin LTO，仅为该最终链接生成map，不执行ABI程序。两路原bytes、真实exit/EOF、源码/配置与最终图像/map前后身份分别保存；超时或未知收尾保持partial，后代静止不宣称。独立配置源审通过（`b8e1d3fc0b4e2a73325ff2e5a4ebef79945a22e26bc68d204c66d87d13881815`），root全文读38741B delta并核26当前绑定及11审查输入。

本次正常功能分支push只释放四目标build/link-only原件收集；实际是否链接成功与最终地址链/元数据另记，没有运行许可或ABI runtime通过。Windows modified配方A的workflow未修改，不由本次提交额外触发；原自然构建失败保留。

### 四目标最终链接实际结果

`f5d9ae50`正常push对应[Linux run38880607323/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38880607323)与[Windows run38880607150/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38880607150)。两平台x64/i686专用build与上传步骤均success，ARM专用步骤skip；采集时完整package仍在进行，不能报告完整包通过。

| 目标 | Artifact ID | 最终图像字节 | 最终图像 SHA256 |
|---|---|---:|---|
| Linux i686 | 12037925332 | 450776 | `fb2817df08fc3683bef1a82f42fe37a0a7f4d6b0e36b21a1f380aea81a02e4d2` |
| Linux x86_64 | 12037078404 | 460336 | `b77c8f1a38de940267d58bcef4724b94529eda65203ddeda674c497e0f178875` |
| Windows i686 | 12037710605 | 134656 | `ba2b29ad8f7a56b3e99bb93edd176aa188da4f4b845e08c859b91475ebe7dbc2` |
| Windows x86_64 | 12036449351 | 157184 | `6120db8179d4d93bf8428601a256b1463e2244dc47a2cf0c2bcaf654fc5443e6` |

四ZIP各9固定成员，root逐项核CRC、提交身份、四源与六配置before/after、同次图像/map四处hash及ELF/PE目标头。原stderr的实际Rust1.96调用为non-test bin、opt3、thin LTO、strip=debuginfo，四编译退出0且双路EOF完整。原件与根核记录在`target/publication-direct-abi-delivery-root-20261016/ci/`。这里只关闭实际最终链接/原件收集门，后继最终地址链与元数据只读复核独立进行，runtime全部NOT_RUN、保护not-qualified，ABI正负控、性能、shipping与真实会话继续开放。

最终图像已完成独立只读复核（报告SHA256 `626ef2a0d69c08777bdee26c02d8c6c95fccdcf6465c0b40cf6daa767a8c2af9`，receipt `a3597b4db1d8035f1430f8cc9aa355f21eea33b7d05681b41c9534899a231a69`）。四条实际typed caller→probe/槽→invoke→entry→SSE2/SSSE3 leaf地址链、SP捕获/独立恢复、ret8、Linux i686 GOT及Win64最终Version1 SEH均与同包map对应；12次既有SDK reader原exit0/空stderr。root全文读报告，核93冻结项和60对原输入/副本，并抽读最终关键指令与Win64 entry展开表。Win32 reader正文仍unsupported I386，控制流保护与实际OS策略也未获资格；未运行图像，正常return正向和精确负控runtime继续待验。

后继API确认同提交的Linux38880607323、Windows38880607150、macOS ARM64 38880607220与主CI38880607183均terminal success，attempt均为1；原快照在`runs-fourth-after-interruption.json`。这是现有完整构建/测试/打包流程的终态，不覆盖此前进行中观察，也不把尚未运行的私有ABI或鸿蒙真实远程会话计为通过。

### 精确图像的受限 ABI 运行接线

现有两个workflow的原capture/upload步骤内加入固定`build`和`abi`两阶段；编译仍900秒，ABI仅以30秒运行staged图像和唯一`abi`参数。执行前必须与上表四个已审最终SHA逐字匹配，同时核同次map、四源及六配置；不匹配保持NOT_RUN并失败，不自动接纳新图像。执行后再次核身份，build/runtime资格和首错分别保留。只有真实exit0、双流EOF完整、stderr空及全部精确输出才能通过；77、异常、超时或partial均非PASS。

输出按冻结源码的精确顺序、case与mask校验，i686为28行、Linux x64为36行、Windows x64为84行，含两个kernel的精确坏callee校准及各九个正向像素case。实际Linux嵌入Python纯parser通过80个接受/拒绝样例，Linux AST/bash-n及原工作流范围检查通过；Windows固定表仅静态核对，本机无pwsh，不能报告其grammar或运行通过。每目标运输原九件加ABI双流，共11条固定路径。

独立源码审查通过（`5a1ae0fa4ecd8c1a220418643c95900e49ef2d29ab60889dbef35b1509d1cc44`）；root全文读61512B增量及作者/审查报告，核56作者绑定和16审查绑定。当前只释放默认x86 CI runner下这四个精确私有图像的一次受限normal-return测试，不修改保护配置；control_flow_protection仍not-qualified。实际四目标ABI结果另记，不由parser、静态编译或运行接线推导通过。Win32 reader展开、full unwind、shipping内部fastcc/inline、性能与live继续待验。

### Linux 两个精确图像实际 ABI

`01cd9bc8`的[run38896199176/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38896199176)已实际执行两个staged ELF，均与f5已审图像及map逐字相同。宿主为x86_64，分别运行i686 ELF32和x64 ELF64；不据此声明任意32位Linux系统兼容。

| 目标 | job / artifact ID | 实际闭集输出 |
|---|---|---|
| Linux i686 | 116957141253 / 12044351162 | 28行；SSE2/SSSE3各12项精确坏callee校准、各9项正向 |
| Linux x86_64 | 116957141036 / 12043259780 | 36行；SSE2/SSSE3各16项精确坏callee校准、各9项正向 |

两ZIP各11成员，build/abi原exit均0、双EOF及capture完整，ABI stderr均空；原调用均为精确staged ELF加唯一`abi`参数。root核CRC、四源六配置三阶段与原ELF/map身份，并全文核28/36行。独立实际读数通过（报告SHA256 `54d184abeeac54ccf3041ac8d636427874205c9cb7d0d6455b4a1cbbdc992f03`）；原件在`target/exact-abi-and-failed-wsl-delivery-root-20261016/ci/`。当时完整包job仍进行；这只关闭两个精确私有normal-return正负门，保护、full unwind、shipping实例和性能继续开放。

### Windows 工作流长度修复

同提交的[run38896197513/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38896197513)在创建job前completed/failure，没有可读check-run annotation，未执行Windows ABI。实际YAML中该run为23547字符，超过[GitHub规定的21000字符上限](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idstepsrun)；此为已复现的配置违规，没有将其称为已取得唯一服务端错误正文。

仅把原run机械移入`tools/verify-windows-publication-abi.ps1`，workflow保留105字符固定调用。前向/反向字节比较证明除闭集Architecture参数、两处相等路径替换及脚本自身配置绑定外正文保持；Windows配置由六项增为七项，原SHA门、预算、capture和11原件不变。全仓73个run长度检查RED→GREEN，当前最大19944字符。独立源审通过（`41fb15f3db66b5dca6b56c1e8241384f8257447c5eab0e76e867717a76c485e3`）；没有安装/调用pwsh或执行图像。后继仅正常提交Windows修复并验证第一次实际Windows运行，原Linux和A运行不重启、不dispatch/rerun。

### Windows 重链接指纹拒绝的实际原因

长度修复提交`632e2e2cba9e14c062e7444c0c2c3cb4b4101584`对应[run38897543788/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38897543788)已正常创建三个架构job。x86 job116961545172与x64 job116961545617均实际build0，terminal/双EOF/capture完整，四源与七配置在build后和启动前稳定；两个任务都在全PE SHA准入被拒，ABI started=false、runtime=NOT_RUN、两ABI原始流均为空。不能称为ABI执行失败。

新x86 PE为`2703dc7d8270968c533f8bea7ad5f5818313346c20e84bebea313976dc5fcbc1`，新x64 PE为`fa1f9903704859af81516fec7ebb8ef5af2bdfb5c62c8242db27a1a22fee428c`。root下载两个各11成员artifact12044621487/12044750934，核API digest、CRC和全部来源/配置/文件身份；独立审查正式解析PE目录后确认，两架构分别仅27/28个字节变化，严格属于COFF时间戳、三个Debug目录时间戳和RSDS GUID，其它所有PE bytes逐字相同，map仅Timestamp行不同。不推断GUID的具体生成机制，也不屏蔽这些字节或自动更新准入表。

独立诊断报告SHA256 `7ce8677fe6e3d6d6bdad020ca6b9d5eac6f46ecd9130cf635d2a387fd786c1f0`，root全文读并核7冻结项与51原输入。原件在`target/windows-abi-run-length-delivery-root-20261016/ci/`。后继改为认证取回f5固定run/两个artifact ID的原九件，逐成员SHA校验后直接运行原已审PE；原build身份与当前runtime身份分开保存。接线尚待源码审查与实际执行，不引入新PE准入或重新链接的确定性假设。

原Linux run38896199176/1后继API已确认整个流程terminal success，覆盖其现有构建/测试/打包及两个已述私有ABI实例；它不替代Windows或产品最终调用实例的资格。632e2e2c主CI38897543804/1也已terminal success；Windows两x86任务本轮保持failure，ARM64包仍按原流程继续。

### 固定原 PE 复用接线源码

仅修改现Windows workflow和固定verifier：沿用已有download-artifact pin，以只读actions权限固定获取run38880607150/1的artifact12037710605或12036449351。下载前要求新输入目录，脚本随后逐项核原九件闭集的完整SHA；认证下载成功或action的digest warning均不能替代强校验。原PE两完整allowlist不变，不/Brepro、不归零或屏蔽元数据、不回退重建。

原九件保留其build-only identity及build双流原bytes；新增runtime.identity.json及ABI双流，共十二个literal上传路径。当前身份明确build_reused=true、abi_build_in_current_run=NOT_RUN及source run/commit/artifact/identity SHA；当前四源/七配置前后稳定与原f5来源分开核，五实质构建配置必须相等。唯一执行仍是原已审PE加abi/30秒，原完整28/84行输出parser逐字不变，raw双流、终态/EOF/首错及保护边界保持。

作者7244B报告SHA256 `7ef766dbad71c8818c57a85640ec8c87c60b8cf4d3623d0acec62e026b0a238c`；独立源码审查报告5424B SHA256 `17d5dac0a9142867e0ef8a9dc1912e4fa072c74584572902576cd1452ab916a5`。root已读完整当前源码/两源delta与报告，核40作者绑定、12审查输入及18原成员SHA。54个host纯数据模型通过、74个run长度合规；本机没有pwsh，未把这些检查称为PowerShell provider或实际Windows执行通过。只释放下一次正常Windows功能分支CI执行，真实下载/十二原件/同PE及完整ABI结果另记，Linux/A不重启。

### Windows 两份原 PE 实际 normal-return 通过

`d5133db58797bbbb6fa3cb34f5ffb7daafd63563`的[run38900650959/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38900650959)中，x86 job116971777185/artifact12045369929与x64 job116971776865/artifact12045484367的固定下载、实际ABI及上传步骤均success；当时完整package仍运行，不据此称整包通过。

两ZIP各12件，API digest/CRC及全部解包bytes相符；原九件与f5原包逐字相同，原PE完整SHA仍ba2b29ad…/6120db81…。当前四源、七配置及两目录各阶段原九件SHA全部保持，旧build身份和当前abi_build_in_current_run=NOT_RUN分开。实际调用均为绝对staged旧PE加唯一abi，30秒预算内native0、双EOF/文件关闭/capture完整、无错误；完整stdout分别28/84行，每个kernel12/40个精确坏callee状态校准和各9正向均通过，stderr为空。像素和canary断言来自同SHA程序正常返回链，未声称stdout另有完整像素转储。

root全文核原输出及身份，独立实际审查5483B报告SHA256 `824fc8b287c7fc62902d98e03623d0c01a396671a16174844607383717b6c5a5`；核16关键绑定。原件在`target/windows-abi-source-reuse-delivery-root-20261016/ci/`。这只关闭Windows两个精确私有address-taken正常return实例，至此四x86目标该窄门齐备；保护仍not-qualified，后代静止NOT_PROVEN，Win32/full unwind、shipping fastcc/inline、性能及真实会话继续OPEN。

### 普通 publication 微基准源码与 metadata

新增唯一non-test `rdp_publication_benchmark.rs`，直接复用真实SSE2/SSSE3叶与完整当前API，旧scalar原文和冻结record165作参考；15counts×两组实际地址mod16，三路同边界、API单列，固定8warm/31原始样本/120秒，准备、全byte/source/canary验证及输出均在计时外。它不更改产品分派或codec广告，当前没有真实计时结果。

初审发现explicit-path装载真实dispatcher会查错x86子模块；固定Rust1.96实际单目标E0583 RED保留。只给生产pixel_convert.rs增加33B显式子模块路径属性，移除即等于原文件，ARM声明、数值、API、实际leaf和四个ABI冻结源保持。修后四x86 example均实际metadata exit0，唯一真实rustc调用为bin/dep-info,metadata、精确目标且无--test；不等于目标机器码、链接或运行。独立Fix1复审报告SHA256 `acfb895e59b61ecb1c6979d50daa862f9ef75ef92deeecba2184738bb17a46ef`，root全文读并核40作者RED/Fix1冻结与11审查绑定。

最初bare Homebrew cargo不理解+1.96.0、随后launcher配对报E0463的两批失败原件均保留，没有用后继正确工具身份追认旧子进程。普通frd-protocol-rdp x64 Linux cross check在aws-lc/libz前置因缺x86_64-linux-gnu-gcc失败，尚未进入产品crate本体；原生CI/完整产品检查与四目标真实benchmark仍待后继，未安装工具链或添加镜像harness。Task4A2.10C不勾选。

### Fix2：完整格式检查的 ARM 模块路径

`d549abf74049dc5749cd5197eabbf28f88914597`的[主CI38903095354/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38903095354)在Rust formatting job116979901934失败，后续test job跳过。原固定1.96 cargo fmt --all -- --check递归解析explicit API模块的aarch64声明，查找src/aarch64.rs而非真实src/pixel_convert/aarch64.rs；不把这轮CI报告为产品回归通过。

仅再加37B的aarch64显式路径属性，x86属性保持；移除两条属性后全文等于原ee562309…，实际NEON/x86 leaf、API、数值与benchmark a473358d…不变。固定同目录cargo/cargo-fmt/rustfmt/rustc及PATH/RUSTFMT后，完整全仓同一格式命令实际RED1（stderr174B）→GREEN0（双流0B），没有skip_children、没有写全仓或改其它格式。作者3093B报告SHA256 `f7f637f1d4c1ca72e91bf38f80023c2d31332bbce954548b18c69489e8c4ba90`，root已读并核18冻结项。Fix2只证明完整格式门，不新增metadata、ARM机器码、性能或产品通过；后继普通原生CI仍须分别取得终态。

Fix2限定独立复审通过（报告SHA256 `d92b747d34c851b85871b73dd3b7cd036deebd05124b974f75ce120fade44a71`）；root全文读3517B并核9关键绑定，确认唯一37B属性、12保护输入及原RED/GREEN工具/命令/终态。只关闭源码与完整格式解析门，后继原生产品CI、性能与shipping保持未验。
