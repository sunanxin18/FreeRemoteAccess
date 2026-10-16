# Windows ARM64 固定 H264 核的来源与容器身份

2026-10-14。`9e773ce` 引入独立 COFF/PE 校验工具，`7c50370` 和 `42d3403` 修复两项独立审查发现；第二轮复审 Approved，开放 P2 已全部关闭。这是 Windows 新配方的先决接口，不是 Windows 修改版 FFmpeg 已构建或可发布的结论。既有 Windows i686、AMD64、ARM64 upstream 包和默认行为未改。

## 接口与固定身份

工具从实际 ARM64 COFF 符号、独立源文件端点证明、同次链接 map 与最终 PE RVA 关联私有函数。固定 `ff_h264_luma_dc_dequant_idct_neon` 为276字节，SHA256 `1e70d378b34fb196afd0942b9bd0c31c1715b4042c6c3e8f032dad5e76b42476`；没有修改源配方、原数学代码或核 pin。无法用真正 function Aux 证明范围时，使用固定原源码与受控编译器重放/私有起止标记证明；所有可执行 section 和原对象保持一致，不能把相邻符号距离或 section 大小当函数长度。

最终 PE 检查独立枚举 normal import FirstThunk、delay IAT/module handle/unload、TLS index 与已支持 DIR64 relocation 的静态写入范围，禁止与核重叠。IAT directory 缺失或错指不会绕过权威 descriptor。普通 TLS 和合法 zero-fill 数据保留；未支持的 load-config、CLR 等机制明确拒绝。此范围不证明任意 TLS callback/DllMain 的程序行为。

源输入门拒绝 label、semicolon、动态 macro 和 quoted opcode 中的外部 assembler 读取；quoted operand/comment 仍合法。CPP 前直接验证物理 config.h 的生成配置域，拒绝 raw 汇编、行号重定向及外部 CPP include/pragma。实际依赖来自同次受控 Clang 成功调用的 `-H` physical include trace 和主源码 argv，再逐一绑定配置/归档 bytes；不使用可伪造的逻辑 `#line` owner 或 record 自报的 trace 授权。

## 已执行的验证

- 初始30项 COFF/PE/source/map 合同测试及实际安装 Clang/lld/SDK llvm-ar 生成的容器夹具；只属交叉编译与格式证据。
- 第一轮41项受影响 suite，通过 normal/delay/TLS malformed/range/overlap 与正常布局，并先用真实 Clang 确认5种外部 reader 配置可编译后验证拒绝。旧 helper 复现9个应拒绝却接受的 subcase。
- 第二轮只运行14项实际受影响 assembler/source suite，38.308秒全部通过；旧 `7c50370` 复现4个 quoted opcode/owner spoof subcase。重复/guarded physical include、伪逻辑 owner、失败 CPP、合法 literal/comment、固定 source→object→map→final chain 均覆盖。没有为扩大计数重复数学或 PE 宽边界测试。
- Root 独立核对最终两个报告、三文件哈希和冻结归档；PE class 与第一轮快照逐字节相同。独立复审核对修复及其新增边界，没有替代或重复作者 suite。

冻结 `42d34030c0785c482a8cf69058de398f632b7cd2` 完整源码和身份清单位于 `target/windows-kernel-fix2-20261014/`。最终 helper SHA256 `4a6e2313d27df2803b0baac80949b44a6aff9ee7c7c4aa24f4379638771261a8`；tests `ef37444924063bb604193a066cb7e877d136873daca9d74091af6c13c0e9e9f6`。本轮未执行 Windows 原生代码、改签名、使用远端凭据或改变产品广告。

## 后继必须完成的工作

真实固定 llvm-mingw 编译器的 generated config、自然 AS argv、COFF、GNU link map、strip/installed/package DLL 必须形成同一次构建的实际链；遇到本工具支持域外的结构须保留失败并审查扩展。还须 Windows 私有保护页/QPC/非叶 ABI 与 unwind 夹具、同 shipping 对象的原 C 数值/30分派/ABI/基准、普通最终包真实装载与 codec fixture。Windows 包装/安装器受信 verifier 和对应源码材料尚未接入新配方，完整软件 codec 与真实协议互操作也未完成。

## 2026-10-16：显式 Windows 配方的源码与宿主接线

`build-ffmpeg-windows.ps1`新增明确的`frd-h264dc-aarch64-v1`选择，仅允许ARM64；未知配方、其他架构及修改配方的既有RunNativeTests入口在副作用前拒绝。默认upstream的x86/i686、AMD64、ARM64与其他平台流程保持。修改配方使用独立build-side目录和fresh attempt，保留源码、配置、对象、未strip DLL、map、安装输出、原调用收据及失败partial；不会向旧stager或installer发布。

Windows source profile及对应源码exact材料已接入，固定上游归档签名只认证上游原件，本地patch单独记录，不能冒充上游签名的修改包。两个新模块分别承担固定工具原argv/cwd/输入输出/工具身份/原始工具stdout与stderr记录，以及同对象到现endpoint/capture/verify的适配。架构与配方策略不进入kernel或平台壳层。当前仍`publishable=false`，不能以source/host结果广告生产codec。

原审查发现所有DLL共用link.map可能被后继链接覆盖；第一轮修复为各module独立map，并从选中原link receipt核对原路径及当前/快照身份。随后复审发现未转义的`$$@`在真实configure的append/eval中提前变为PID；第二轮固定token改为`\$\$@`，穿过真实configure后留给原make规则使用。捕获后未改argv、复制假map或放宽consumer。

初始两个新host suite的10项为9通过/1缺pwsh跳过，受影响source/macOS/Linux 19项通过。第一轮扩展的最终14项为13通过/1缺pwsh跳过，保留原共享map顺序与确定性交叠的真实RED。第二轮只执行真实固定configure相关原代码、原make顺序/交叠的3项定向控制，全部通过、10.555秒；旧token写出PID map的两个实际FAIL保留。未重复未变的完整suite或原数学。pwsh/Pester未安装的两次尝试保留为环境限制，不计通过。

上述configure控制逐字提取固定归档的原函数、完整选项循环和配置写出片段，不是完整configure工具探测、PowerShell/WSL或完整FFmpeg构建。现有本机clang/lld只用于固定276B数据的最小COFF/PE容器和producer→consumer身份控制，不执行PE，也不作为shipping对象。独立Fix2复审`6499ea1662d9ec05086581fc5fc4ddfdb1fc91e823edcb07210fed017485cacc`为Spec PASS/Quality Approved，0Critical/Important/Minor；原map问题在source/host范围关闭。原A/Fix1历史Changes requested与全部失败记录保持。

冻结材料分别位于`target/windows-shipping-recipe-A-20261016/`（10997项）、`...-fix1-20261016/`（21146项）与`...-fix2-20261016/`（136项）。最终清单SHA256为`88560ee05f4508506c34eb88213676afe22fda8510a84558a64e03ecd9573ba9`；root完整读取报告/所有delta，独立核对各冻结项及当前六源，保护工具/consumer/patch/pin/私有门均保持。同期root主计划更新明确单独归属，不声称whole-tree干净。

**实际固定Windows/WSL自然构建仍未执行。** 目前自然AS argv与安装复制后原位strip已知可能超出现consumer支持域，必须保存真实首错和partial后另审，不能等价改写或另编shipping核迎合旧接口。后续执行接线还须保留configure/make的原始双通道及完整固定工具字节，现文字合流日志/工具hash本身不等于这些完整材料。B同对象Windows数值组合、C真实ABI/guard/qmul/QPC与三个fresh-process性能窗口、D修改包/受信安装/实际装载及完整codec/live资格全部继续OPEN，默认upstream和系统NativeFirst不变。


## 2026-10-16：独立执行与原件运输接线

在上述已审配方之外，新增单一Windows执行authority、原始双通道及固定工具原字节保留、隐藏attempt/source-cache全量POSIX运输的源码准备。唯一自然builder仍固定Release/ARM64/Ubuntu-24.04/修改配方，默认upstream路径不变；没有执行实际优化构建，也没有从普通Windows包CI推断修改配方合格。

初版执行层复审发现两项P2：packing追加后缺少完整reader，以及setup中合法降权后代与全组同UID检查不相容。Fix1已修复，并经独立Spec PASS / Quality Approved复审，未发现新问题。原tar/packing receipt追加完成后保持不变，实际完整reader的原始通道、退出和工具字节另存固定verification记录；上传合同精确为三个固定文件。进程归属由稳定父系证明，原supervisor/未reap leader租约保持；自然builder前有两个独立Linux降权/正常结束与HALT检查，失败不会开始builder。pack和最后reader共用原10分钟运输预算。

初版18轮失败与通过记录、完整源码和5856项材料冻结在`target/windows-shipping-A-execution-20261016/`；Fix1的1361项材料及10轮原记录在`target/windows-shipping-A-execution-fix1-20261016/`。独立复审报告SHA256为`dc375e186bc583b07776f156e57860ff10182e0833acf6f1dfd79fde9a3922fa`。root完整读取全部1063行delta、核对冻结材料，并实际运行最终运输模块22项控制，22/22通过。Mac控制不能证明Windows PowerShell或Linux/proc/WSL运行：14项Pester及真实Linux门仍未运行。

下一步由root提交审定源码后，首次受限workflow注册push运行一次固定Windows/WSL执行流程。该push本身会触发，不再另行dispatch或并发重跑；实际结果另记。当前仍未取得自然A构建结果，B/C/D和生产资格继续OPEN。首个自然构建的真实首错、原argv与partial必须保留，不能为通过旧consumer改写工具参数或另编shipping核。

### 首次实际执行：初始化失败，尚未进入自然构建

审定提交`ba6035752f746651681708c07cefa1c896248880`的首次注册push触发[run38847486035/attempt1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38847486035)。checkout成功，Execute在2026-10-16 03:50:24 UTC报`$Name失败`未定义：PowerShell将中文后缀合并进变量名，错误报告本身遮住了初始化原始错误。随后Collect因state尚未创建而拒绝，三固定件均未产生，artifact数为0。没有WSL/setup/control或FFmpeg构建通过的记录，也没有数值/性能结果。

原job日志保存于`target/windows-shipping-A-execution-root-fix1-20261016/ci/release-a-job-original-3.log`。下载过程第一次TLS timeout、第二次CLI拒输出终端转义的失败均保留；第三次仅允许读取原bytes并剥转义显示，不是CI重跑。当前已证实错误插值及早期材料遗漏，初始化的原始失败仍未知；先修复诊断和材料保留，再安排受限验证，不直接重跑自然构建。

### 初始化诊断修复源码

错误插值已改为`${Name}`，外层单独保存首个ErrorRecord，后续诊断失败不覆盖它。当前同一workflow的活动命令固定为`BootstrapDiagnostic`；原Collect和正式POSIX三件上传在该模式下不可达，不将诊断文件伪装成正式构建材料。后继恢复Execute需要单独审查源码和执行记录。

固定六个必需控制包括：真实旧formatter token反例与新错误内容、原gateway的双通道二进制exit7、缺失exe、日志目录冲突、独立scratch的initialize成功和另一个预建root的initialize失败。它们不依赖Pester，不调用checkout、WSL、setup或builder；独立Windows forensic上传精确列出66个可能的文件路径，缺件、变化、EOF未知均按partial记录，始终不授stable或生产资格。

本机仅运行受影响WorkflowAuthority控制（原配置控制RED exit1、更新后1项GREEN exit0）与窄源码/数据检查；24项冻结材料及当前四源已由root复核，完整514行delta已读。Windows PowerShell和六个运行控制尚未执行，Pester明确未调用，首次run初始化原因继续UNKNOWN。源码材料在`target/windows-shipping-bootstrap-diagnostic-20261016/`；独立Spec PASS / Quality Approved复审通过，报告SHA256 `5da7d6b33db5c0ad4f8a33bd6d1156b655f944aec70f78c27454b55b963cc655`；root完整读取并核两项review冻结后，释放一次固定诊断push。实际结果另记，不触发自然builder。

### 固定诊断实际结果：多候选路径拼接

审定提交`dd46ea849f86cee3f5bcc074b41cc05a219c4f50`推送触发[诊断run38849395836/attempt1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38849395836)。四个必需控制实际通过：formatter三种历史token RED/新内容GREEN、双通道各65536字节逐byte及exit7、缺失exe、日志目录冲突。两个initialize控制均在启动子进程前失败，真实record为exit=null、terminated=false；error明确包含两个python.exe路径被空格拼接成单一路径。

当前`Get-Command -CommandType Application`可返回多个ApplicationInfo；直接取集合的Source并传给string路径参数会发生这个问题。资格工具的raw gateway、Windows工具目录收集，以及修改版builder的native gateway有同类读取点，下一修复按自然解析顺序只取首个ApplicationInfo，保持原调用名/参数/PATH，不硬编码Python路径或静默换工具。尚未修复验证成功，不恢复自然构建。

该run成功上传独立forensic artifact12024701847，ZIP 1,240,638字节/API digest `8de9581b55c1da76d206fdcdce0fb7c8293a306d4f94e61a40022214735bb48d`；root读取47成员、核45个存在文件前后身份无差异，逐byte核binary及三formatter结果。原件在`target/windows-shipping-bootstrap-diagnostic-root-20261016/ci/`。这是本轮具体失败模式的实际复现；首次run38847486035缺初始化原件，其具体原始原因仍不能由新fixture追认。正式三件、WSL门、自然FFmpeg、B/C/D及生产资格均未通过。

多候选解析的最小源码修复现已完成：资格工具两处、修改配方native gateway一处均先取首个ApplicationInfo，再取Source；原调用名/argv/cwd/PATH、零候选错误和默认upstream分支保持。host诊断另存Python自然候选顺序；这不是每次进程映像观察。51项源码/原JSON/原日志身份及完整123行delta已由root核对，两个新增Pester仅prepared/notrun。CI物理CRLF源码与本机LF源码分别记录raw hash，只有归一化文本相同，不混称bytes相同。后继仍只运行同一固定BootstrapDiagnostic，独立源审Spec PASS / Quality Approved通过，报告SHA256 `f420a4c7c574378d39f2613ee85c79ad801eb583d399d2f25b5dfb35f7205dae`；下一实际GREEN尚未取得，root仅释放一次固定诊断push，不恢复自然builder。

### 修后实际诊断：六项全部通过

提交`373c33ba0f846fc526a8d742a008dfc810f69c40`的[run38850194932/attempt1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38850194932) / job116809495226已SUCCESS。六个mandatory均实际PASS，initialize正控exit0并产生身份正确、尚未开始checkout/setup/builder的state；预建root反控真实exit1/FileExistsError/无state。host记录仍有两个Python候选，首项为toolcache Python；两次initialize raw均保存该首项exe身份，并观察退出与EOF，前一诊断的多路径拼接RED已由本次GREEN解决。

独立artifact12024329621为1,455,028字节，API digest `b2c070e44143406d20a8ecd2319d77c63d63468d7a01eb81576febadbb38d5e5`。root核对46成员/45个存在文件前后身份、三个formatter历史RED/新GREEN、binary逐byte、两个initialize真实exit/state/stderr，未发现差异。原件在`target/windows-shipping-bootstrap-resolution-fix-root-20261016/ci/`。材料仍forensic_only/stable=false，不是正式POSIX三件；Pester和catalog/modified-native对应新增控制未实跑，不能以raw gateway成功替代它们。

该成功仅关闭初始化诊断门；原首run缺失的原因不追认，WSL/Linux进程门、完整自然FFmpeg及B/C/D继续待验。后继恢复自然入口时须保留失败时的早期initialize原件，不能再次让缺state丢失最早错误。

修后六项实际结果经独立读数复核通过，报告SHA256 `501844b950479562a4762792d64e84512305c6a9cb681ce12103dfbeac2cfc7b`。恢复自然入口的两路径/130行delta、16项冻结及独立源审`5063bf135932d7f5e03033903213d7d00f26f2de09eec4f5bd1b4e29bc5af4bf`已由root完整读取/核对。现在只释放一次固定Execute流程；Collect和正式三件always，forensic仅失败/取消时尽力运输73个精确可能路径（含最早initialize七原件）。该恢复提交的push本身触发，不另dispatch或并发重跑；完整自然执行结果另记，B/C/D继续开放。

### 恢复完整流程后：checkout 目录冲突

提交`263d7bf04bea7f1c13a36154cb053cf4e372c1e7`的[run38851041806/attempt1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38851041806) / job116812080832已实际运行：initialize成功，checkout子进程exit1，退出与双通道EOF均已观察；WSL与FFmpeg尚未开始。失败forensic artifact12025486411为4,949字节、digest `5ce56fea60505843274e30308fcb5dd0919a549acba906acac9ff2f4713b642f`，仅带回外层首个ErrorRecord；checkout原stderr未在旧精确清单内，不能声称已经取回它。

本机直接调用当前生产`initialize → canonical_checkout`，在Git discovery和Popen均为0时真实复现`FileExistsError`：initialize已创建空`checkout-inputs`，checkout再次无条件mkdir。不存在的scratch repo未被创建，输出仍空。该目录生命周期冲突是确定的源码问题；本机trace与缺失的Windows原stderr保持区分。预检冻结21项，报告SHA256 `2a14c494993d71fbead2120ac059ead00064b15ccc6a1bcb1cf6953b0a46c18a`，root已全文读取并逐项核对。

最小修复只允许同次初始化的空真实目录，继续拒绝非空、文件和symlink；不改变Git参数或blob校验。后继固定前缀诊断复用原Execute至工具catalog与fresh检查，随后在第一条WSL调用之前返回；正式三件仍缺失，前缀原始日志单独运输。当前处于源码实施阶段，尚未取得Windows前缀GREEN，不继续盲跑完整自然构建。B/C/D继续开放。

目录修复与封闭前缀已完成源码实现：本机 focused 5/5、完整运输模块26/26通过，含真实initialize后隔离Git的CRLF/属性/blob控制。独立审查发现一项P2：固定54路径内的两个隐藏源会被upload默认过滤；仅forensic启用`include-hidden-files: true`并加精确断言后，该控制实际1项RED→GREEN。原26项无需无关重跑；旧日志与冻结保持。root已全文读取411行原diff、20行修复diff及两报告，核139项初版和15项修复材料。此处仍是源码/宿主结论；Windows前缀、实际隐藏文件上传与WSL/FFmpeg未由它替代。

该唯一P2经定向复审 Spec PASS / Quality Approved关闭，报告SHA256 `27acf8154505e130c95e42173ad2de2361dab2fcf831b7fa491d3aec0fd1e2e3`。root核对复审两件及四当前源后，只释放一次固定`BootstrapCheckoutDiagnostic`的正常功能分支push；不另dispatch/rerun，不启动自然A、WSL或builder。实际Windows结果另记。
