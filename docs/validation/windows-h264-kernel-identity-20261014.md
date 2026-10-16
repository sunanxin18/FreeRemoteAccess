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

### Windows 源码准备实际通过

提交`0eaff068ddc2391d025f559d771aa02ca18cb93a`的[固定前缀run38853042725/attempt1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38853042725) / job116818287351已SUCCESS。initialize和checkout实际exit0、退出及双通道EOF均已观察，stderr为空；state为checkout_ready=true、setup_ready=false、builder_started=false，未启动WSL。初始化实际观察到Python MainModule；checkout快速进程未观察到，原因照原record保留，不能合并称两次均有映像证明。

artifact12026220958为4,117,845字节、SHA256 `d7ea452d9ce0f2d12c64edd4918d337f5e6c1bd5ec6417eee6a0b64ee9c8587f`，实际42成员。root核对全部16个物理源码副本等于同提交blob，包含先前漏传的两个隐藏源；21条Git原调用均exit0且exe身份一致，六个Windows工具的catalog与保存bin身份相同。54允许路径中缺失12项均为本次未生成的错误文件、已移动的初始化原位置、未观察到的checkout映像和原不存在的attributes-before。原件在`target/windows-shipping-checkout-diagnostic-root-20261016/ci/`，未执行下载内容。

这关闭了本次源码准备冲突及隐藏源上传遗漏的实际门；完整WSL环境、自然FFmpeg、正式POSIX三件和B/C/D仍待完成。

实际前缀的独立核验已通过，报告SHA256 `fa13a86f780e8a9b2c14c7d104d248bc8e834fb4c153168fe5e655278096f4f7`；root已全文读并核两项复审冻结。它进一步确认21次Git中20次观察到MainModule、首次快速HEAD未观察到；不将工具catalog快照误称六工具都已执行。后继恢复完整自然流程仍须独立源码审查。

### 恢复完整流程的源码审查

固定Execute、Collect与正式三件的always接线已恢复，独立源码审查Spec PASS / Quality Approved，报告SHA256 `8fd0404f29d90a287260cf3c0fd3afecf75ab3018f9a3beafd14db587ebc43b9`；root已全文读取291行diff并核16项材料及四当前源。唯一workflow控制实际1项RED→GREEN，其余控制无需重复。失败forensic保留原54路径及hidden开关，再加入154个固定阶段原件路径，总208条，无wildcard；它仍只是有限partial，UUID和动态attempt内原件依赖正式完整运输，不因上传成功升为stable。

下一次审定提交只触发一次原固定自然流程，保留初始失败、setup/control拒绝和原工具调用；不另dispatch/rerun，不改变默认upstream或修改核。实际WSL及FFmpeg结果待记录。

### 最新实际执行：WSL 依赖安装中的进程归属拒绝

提交`59cdd705eaeebbe7c0a69f5f43c1b76ecc672240`的[run38854683516/attempt1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38854683516) / job116823393151失败。initialize、checkout、WSL install/version和路径往返已走通；setup实际执行到APT下载及dpkg Reading database，随后原supervisor的`owned_graph`拒绝“发现逃逸SID/PGID后代”，setup原exit1、退出及EOF已记录。Collect的停止核对也实际exit1，拒绝seal；state仍quiescent=false、setup_ready=false、builder_started=false。FFmpeg尚未开始，不能报告H.264编译失败或稳定运输。

forensic artifact12027216250为6,554,118字节，SHA256 `917532044a816f230998b073d0cb1286f193dfd7eb3cba71dd6970f783748adc`，本次原setup stdout/stderr与cleanup stderr已实际带回。原job log第一次下载失败记录保留（其中临时签名下载地址已脱敏），第二次只读下载成功，37,910字节、SHA256 `a36516a8a33c050747487903eb302fedec61b6a777919c387232ce74de98a1f3`，不是CI重跑。原件位于`target/desktop-qualification-integration-root-20261016/ci/`。

当前原件未包含触发拒绝的具体PID和两次进程图快照，不能把正常APT伪终端、setsid假设直接当作已证实的现场根因。后继先只读分析并准备有界诊断，不直接放宽进程归属、广泛kill或禁用APT sandbox；旧失败历史及正式A/B/C/D继续开放。

### 保留原归属拒绝的固定 APT 前缀诊断

四源码新增封闭BootstrapAptDiagnostic，只运行同一prepare的原APT update/install前缀，在controls、setup_ready、builder之前返回。原owned_graph/identity/信号权限/stop、正常成功end与完整prepare保持；没有修改Use-Pty。失败end附已经取得的原两图、候选和受身份前后核对约束的有限comm/exe后采；固定receipt带回本次callid UUID原bytes/hash，缺失或变化仅partial，双读相同不授quiescent。SetupOnly finally不创建builder配置；156条精确forensic always上传，正式Collect及三件固定禁用。

宿主graph/唯一入口控制RED→GREEN，focused7项通过。完整Python模块只运行一次33项：32通过，1个新增测试临时父目录错误；修后仅该项定向通过，未称一次完整模块全绿。作者报告SHA256 `663b59cf48c26ed6f59c6051fd226dfeeeb08870292b06621eb2b197c21c1bb9`，独立审查 `a8b45a33bdf3c040dbb4e517ecdfb0571f36f121574444f93c769b052367d105`；root已全文读取报告和778行diff，核1349项、四当前源和七保护输入及独立两件冻结。以上仅SOURCE/HOST，不是Windows/WSL/APT执行结果。

本提交首次push只释放该固定APT诊断，不另dispatch/rerun、不运行自然builder。实际图、原UUID导出、PS finally、Linux停止与具体违规身份待原件核验；原自然失败、完整A/B/C/D和生产资格继续开放。

### 固定默认 APT 诊断：已取得组外 dpkg 证据

提交`bd4bb248e7a92e8577d67488bce2ddd5a70d1579`的[run38857430630/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38857430630) / job116831938614已FAILED。固定五probe均0，实际APT2.8.3、dpkg1.22.6，Use-Pty窄查询stdout为空；setup和stop各exit1、原EOF完整，state仍quiescent/setup_ready/builder_started=false。没有进入controls或FFmpeg，正式三件skipped。

这次117成员ZIP为7,769,624字节、SHA256 `02d4eb688fd4eac042074cfb2bfcfaa369ff27be6c00e2f44bd2e6da92a2a625`，artifact12027189149。root核全部CRC/156精确allowlist及16同提交源码；四UUID通过固定receipt实际带回，nonce/callid/source c44d65绑定、base64原bytes/双读SHA均匹配。两图都显示 ancestry `547→548→844→895→897`，895/897的SID/PGID均895，偏离leader548；四次后采身份绑定分别取得dpkg、dpkg-deb的comm/exe。这是后采观察，未直接记录setsid调用，不追认旧59 run的缺失身份。独立实际读数报告SHA256 `0bfa896e3f6a7ed219322bcc4de5cef6a99ad85a227d796afcbecc5633d17b40`已root全文读取/冻结核验。

原job log两次只读下载均网络失败，没有成功原job log；已保存下载退出和脱敏stderr，不能用其补写现场内容。上述事实均来自原ZIP和API。原件位于`target/windows-shipping-apt-prefix-diagnostic-root-20261016/ci/`。

root另从[Ubuntu官方APT2.8.3源码](https://archive.ubuntu.com/ubuntu/pool/main/a/apt/apt_2.8.3.tar.xz)只读检查：`Dpkg::Use-Pty`默认true，PTY子路径调用setsid；归档SHA与同目录.dsc匹配，未验签、未执行下载内容。该机制与现场组外分支相容，仍属基于源码和观察的推断。后继仅准备安装命令的Use-Pty=0窄候选，保持所有权/信号/seal及sandbox；必须fresh host/distro和真实dpkg工作量验证，不在已变rootfs重跑冒充RED→GREEN。

### setup 显式关闭 PTY 的最小修复

共享prepare安装行只增加`-o Dpkg::Use-Pty=0`，对应原单项prefix控制实际RED exit1→GREEN exit0；workflow仅说明注释改变，字面BootstrapAptDiagnostic、156路径和正式三件false保持。没有改sandbox、包列表、原进程归属/信号/stop或自然builder调用，也未重复另外32项宿主控制。

作者报告SHA256 `bef01060bc298b396f0c5caa55e285ad649fd5cb57d1bb6ccc2e07aa39f0d708`，独立审查 `4bebb3c42e172722228998bea31c564e2400caf498ef2dccb9beb08486776516`，root已全文读37行diff及两报告，核19项材料、三当前源及独立两件冻结。当前仅SOURCE/HOST，修后实际APT尚未验证，不追认旧失败成功。

下一次正常功能分支push仅运行同一固定prefix，使用新的host/Ubuntu实例，不复用旧rootfs；须保存版本/hash对照和真实dpkg解包/配置工作量、setup自然0+EOF、end child_exit0/quiescent/reaped及原stop0/stopped证据，再由root独立核验。环境漂移或noop单列，成功也只关闭APT前缀，完整setup/LLVM/FFmpeg和A/B/C/D仍待验。

### 修后 fresh APT 前缀实际通过

同提交`6b95c81e`的[固定前缀run38871211074/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38871211074) / job116875482251成功。新的host/Ubuntu安装原件及stdout证明确实执行48次解包和48次配置（6 upgraded/42 newly installed，含编号46项及前置libc两项），非noop。五probe原stdout与默认RED逐字相同，APT2.8.3/dpkg1.22.6及exe指纹保持；删除唯一Use-Pty=0选项后prepare与旧默认原件相同，supervisor c44d65未变。

setup与stop自然exit0且完整EOF，四UUID和同nonce stopped原件匹配；end child_exit0/reaped/quiescent/error=null，state firsterror=null、apt_prefix_completed/quiescent=true、setup_ready/builder_started=false。last_uncertainty保留早先成员快照不稳定历史，未抹去或误报当前错误。setup stderr仅locale生成通知；不能按stderr非空推成失败。

artifact12033647017为10178868B、119成员、SHA256 `3ccdae6435773b36a343689fc60dca1a73c2aa96703979ca2d3ba28db982914a`。root核119CRC/156allowlist/16提交blob、五probe、精确选项delta和四UUID/stop，并全文读独立实际报告SHA256 `ab125882a26d091d8dd8ac97d75295f1f3ffc9ef2529c76f5107e62f4d61c73d`及两件冻结。没有成功原joblog，也未称forensic是正式运输。这里只关闭APT前缀门；完整setup/LLVM/FFmpeg、正式三件、B/C/D继续待验，原默认失败与旧59未知身份不追认。

### APT 验收后恢复完整自然流程

仅workflow与唯一WorkflowAuthority主控制恢复原Execute/Collect/正式三件always；失败/取消时的原208字面forensic逐项逐序保持。Use-Pty=0共享prepare、qualifier/builder/consumer及原bootstrap源码未改、不重跑。原主项实际RED→GREEN，独立源审报告SHA256 `3b2b84f6b11c7eba18fefe72f24674eaf1d06ec204ef5d9006df1b560f63490d`；root全文读35e9作者、287行diff及独立报告，核17项及两件review冻结。

本次正常功能分支push只释放一次原固定自然A，不另dispatch/rerun；新调用须自身完成完整setup、LLVM、controls、FFmpeg及原正式运输。原208 fallback在正式运输失败时仍不保证动态UUID/attempt完整，缺件保持partial/unknown。APT前缀与Windows私有publication guard成功均不预授这次完整自然构建通过，B/C/D继续开放。

### 自然恢复后的 checkout 首错

59e620e12686053e7416bd2f15541c9b46e457ac 的自然run38872975021/attempt1、job116881223871已失败。initialize0、checkout1均完整捕获原双通道及EOF；首错为`canonical_checkout`对`tools/build-ffmpeg-windows.ps1`的regular/非symlink/物理bytes等于批准blob合取校验拒绝，发生于第一条WSL调用之前。Collect随后无WSL路径只能保留partial，不替代原始首错。

原artifact12034111596（926371B、21成员、SHA256 `f0dc37d2bda7d4e6903214cfa06153f8cfdc472045424878ab16721e5c6d7377`）CRC及allowlist通过；10条Git调用均0，HEAD/tree正确，16源text均unset。批准builder blob为61041B、SHA256 `16a4e460d544296229c99d020e4f433c6a68574e59ff741da7d7ec2a095e89bf`；失败现场physical builder未收入包内，不能据此判定缺文件、链接或CRLF差异。独立实际报告SHA256 `cbc51ce2b35fd7cdc4bb4760293d5aee44a70f8c0cb85138ede3386feb93c984`已由root全文复核。

本次没有APT、LLVM或FFmpeg执行证据，state.quiescent=true为早期默认值，不能当作Linux停止证明。此前不同run的fresh APT前缀成功单独保留，完整A及B/C/D继续开放。

### 固定源检出缓存修复

隔离macOS仓库实证：初次CRLF checkout并建立与物理文件匹配的index stat，排除同秒racy路径后，切换info `* -text`再执行原`checkout-index --force --all`仍保持20B CRLF文件，未生成18B LF blob。立即执行的首轮曾重写成功，原结果保留；这说明旧算法依赖文件缓存时序，不代表现场失败文件已被证明为CRLF。[Git官方entry.c](https://raw.githubusercontent.com/git/git/master/entry.c)亦在stat未变时先返回、再判断force；该源码仅佐证机制，没有冒充现场Windows Git版本绑定。

最小修复只在原Git命令前新增6行：先全部预检固定16源为regular且非symlink，再仅移除这16源，由原checkout-index重新生成；HEAD/index/tree、属性、原argv/工具身份及最终physical bytes==blob门保持。现有单项增加真实cached CRLF条件，生产helper实际RED（ValueError）→GREEN；完整transport模块一次33/33通过，未列文件bytes/inode/mtime保持，后部固定路径为目录或symlink时在任何evict前拒绝。验证只在macOS隔离fixture，真实Windows自然流程仍待验，没有新增诊断模式、上传路径或工具参数。

独立源码审查通过（报告SHA256 `d442aaec5614af5e59f212d6d73b4307169a9776fa673c617bf7791f65629f5f`），root全文读作者/79行diff/原RED、GREEN、33项日志并核30项关键原件及两件审查冻结。workflow仅更新一行说明，去注释合同、208固定路径顺序及push范围保持。现正常功能分支push只释放下一次原Execute自然流程，不另dispatch/rerun；每一后续构建与运输结果须由新run自身证明，原失败及现场缺口保留。

### 缓存修复后实际通过检出并进入 build 入口

[run38875212085/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38875212085)、head73f2de9e、job116888494994最终failure。原151成员forensic为11320934B、SHA256 `89f6c68868dabd1db51e42f95a9a395e27304fb5338eec26fa443b826b78ec90`；root核16提交blob及checkout/setup原exit0/EOF。完整setup包含APT和LLVM归档指纹检查，原Linux normal/HALT controls及清理有对应原件；这关闭本run的checkout/setup门，不替代完整构建。

唯一builder原pwsh自然exit1，双流EOF完整；222B stdout给出保留attempt路径，843B stderr仅为第738行通用“FFmpeg build失败”。绑定源码定位在原`make -j4` Invoke-Wsl门，尚未到install；没有内层原wsl/native argv、exit、stdout/stderr或动态attempt树，具体make/driver/ASM/capture/supervisor首错仍UNKNOWN。不能从外层非零预判组件根因。

Collect预算为300秒，约5分钟后124，原stdout/stderr均空；三个复制阶段没有进度原件，具体耗时阶段未知。固定scope停止报告不能替代全量copy完成，state仍quiescent=false/attempt=null，原first_error保持builder，正式三件缺失。独立实际读数SHA256 `cb9856da24b79188b7d495fbc97545aae16153daeec771b584b444561221d248`与只读预检`d110fa81d012f49db095d0561d1fe4ad41fb365e971773326e612a3393fbbff7`已由root全文读/冻结核验。

后继原joblog已取37448B、SHA256 `02e64160d58485ab9463513f9754e90b88dadc300153c6d77470cac5f6ef43ff`；此前fifth API仍post-checkout，后续final-check才确认整个job终态，不覆盖旧快照。原log第一次下载被gh的ANSI输出保护拒绝；核帮助后仅允许raw bytes进PIPE文件，未在终端渲染。当前仍须先补关键失败原件及有限收集阶段证据，再决定构建修复，不放宽原stop/consumer或以增加超时猜根因。

### 关键失败调用先保存的源码修复

Collect在原退出/EOF、工具身份与stop门通过后，先导出唯一绑定的失败`FFmpeg build`调用，再进行原大目录复制。固定configuration、attempt/PID、call-id/nonce、原native argv/cwd/双流与supervisor/stop相互绑定；失败歧义、越界、链接、读取变化或超限只保留partial。总源上限40MiB、双stream各16MiB，旁路在正式payload外，原消费者与stop合同不变。失败forensic在原208条后只追加25个固定路径，无wildcard。

原三条复制/工具收集命令及顺序保持，只有固定三阶段start/end/exit记录；导出和复制共用原300秒预算。审查发现并修复了end marker失败掩盖原命令退出的问题：实际shell反例先重现六个双失败错误退出，修后15组合均保留应有退出，marker错误仍进入原stderr。focused9项、修后完整运输模块40项均exit0；实际inline shell的bash-n和Python AST通过。原39项结果与原错误报告保留，未用新结果回写旧证据。

root全文读原三源增量、Fix1报告/5081B delta及独立复审，核33关键原件、127修订原件与复审冻结；最终独立报告SHA256 `89fe2b7bd66f93eb3f924c17c53e67172745321c786da49ca349ade038ac8e8b`确认原P2关闭。原件在`target/windows-shipping-A-failed-wsl-export-fix1-20261016/`。本机没有pwsh；Windows PowerShell/reparse/WSL和真实A仍须由后继运行证明。

本次正常功能分支push释放下一次原固定Execute调用，先取得真实内层错误再决定编译修复；没有预修make、增加重试/超时、放宽guard或发布修改包。run73具体make首错与完整自然FFmpeg/正式运输资格继续开放。

## 2026-10-16 原始失败调用先保存：实际后继读数

`01cd9bc870e32f9f49f876d30680f3c0ee960d26`的[run38896199173/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38896199173)已terminal failure。builder仍在原FFmpeg build入口返回1，捕获完整、非timeout；外层只说明原make调用非零，尚无其内层首错。Collect再次300秒超时124，正式tar/receipt/verification未产出。没有再次调用原Execute或以等待超时重跑构建。

新失败调用导出脚本确实执行：Python调用exit1、完整capture、非timeout，selection.json为complete=false、files={}、error=open identity changed。拒绝位置是Reader.read比较路径lstat与已打开fd的fstat签名，现原件没有记录具体失败文件或字段；不能凭常见Windows差异删掉身份门，也不能称真实文件遭到替换。make内层stdout/stderr仍未取得，根因继续UNKNOWN。

新三phase原记录已缩小复制超时范围：llvm-copy与system-tools均有end/exit0；build-root-copy只有start，没有end/exit。这与全量Collect终态124一致，不再泛称三段均未知。错误保存、复制阶段与原make失败分别记账，未据此修复汇编或改变原自然工具调用。

Artifact12044899390含159成员，完整9303878B、SHA256 `171dc6df6efff0368fd917c4c2eb768a724d592f9ff0818df67b77918f21930b`。两次完整GET在90/240秒超时的partial原件保留，后继认证取得同一原artifact的8个Range、同ETag/206/完整总长，重组命中API全SHA和ZIP CRC后才解包；未执行内容。root核16checkout源码与01cd提交逐字相等，原完整joblog38881B另存。材料在`target/windows-abi-source-reuse-delivery-root-20261016/ci/a-range-download/`，独立只读核验已完成（报告SHA256 `96e264a9762f387abf7c00800c24cd2d555363cbc9b1ad094c72972f1730d3b7`，root全文读6612B并核16关键绑定），确认的是来源/失败边界，不是make根因。下一步先取得有限Windows lstat/fstat真实控制和失败字段，再决定最小修复；不先扩大构建或软化身份校验。

## 2026-10-16 有限身份诊断与 Windows 原生观察准备

原七字段 dev/ino/mode/nlink/size/mtime_ns/ctime_ns 严格比较保持。内嵌 exporter 在八个既有 read/copy 比较点仅对实际不等附加 operation、根内相对路径、cap、真实 before/observed、差异字段名及已有文件属性；单项4096B、selection128KiB的上限保持。拒绝后不继续读取或补写未观察阶段；诊断构造/编码/写入失败仍保留原 identity 拒绝及非零结果，成功导出结果没有新诊断成员。原stop/source/唯一attempt、20秒/40MiB、208+25固定路径及正式consumer保持。

缺诊断的 host 反例实际RED1→GREEN0；八比较点、实际host replace及诊断失败注入的focused7项与完整transport模块一次44项通过。这些均为macOS ARM64 Python fixture，不是Windows兼容性证据；原make首错仍UNKNOWN。初版与仅新增四个CI来源字段的修订材料均保留，源报告见 `target/windows-file-identity-diagnostics-source-20261016/` 及 `target/windows-file-identity-diagnostics-provenance-revision-20261016/`。

新的无参数 `tools/windows-file-identity-probe.py` 仅在Windows执行，固定workspace下fresh目录、两类稳定小文件及一次开句柄前replace，记录lstat/stat/open-fstat/read-end/path-after全部原字段、固定payload SHA、Python/Windows版本和四个非敏感CI来源字段。共享30秒，最终三份payload与≤64KiB单JSON；measurement_complete/exit0只表示观察完整，identity_compatibility始终UNQUALIFIED，稳定文件不等会如实保留。

普通Windows工作流仅x64行在Rust构建前运行该小probe，并单独上传唯一literal `target/windows-file-identity-probe/result.json`；它不运行WSL/FFmpeg或扫描旧attempt。本次不修改长A工作流，其push filter仅自身文件，本次不会重新触发长A。Windows实测与任何兼容修复仍待原件，不按常见差异删除字段或改成hash-only。

三源独立审查通过（报告SHA256 `b8fcebfcf8d051c1079d5eb8027b43473dc879d5f4f408b8081f11c128c8d037`，root全文读并核7审查项、11修订项、24旧项及8保护输入）；后继两步接线独立审查也通过（报告SHA256 `875c469f55a6de8cd4ded919a8066a5e7ff1200f6a20684bd5e5cc9824925acd`，root核7项）。删新增589B block后Windows workflow逐字恢复已审基准接线；全仓77个run均未超过21000字符。这里只关闭source/host门，实际Windows观察仍待本次正常CI。

## 2026-10-16 Windows 文件身份原生观察：ctime 语义差异复现

`8cb7f719` 的 [run38909036327/1](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38909036327)中，x64 job117005568749 的固定probe与上传均success。artifact12049878393恰单成员result.json，ZIP1681B/SHA256 `eb15d212bdbe3a8bc94798160e3582a24370bfb0ea8cb9ece7f34e02d77e8774`；root核API来源、CRC和JSON bytes。probe源SHA `cf3f5887…` 精确等于审定LF源码的CRLF检出字节；CPython3.12.10 AMD64/Windows build26100实际记录齐全，measurement_complete=true、identity_compatibility仍UNQUALIFIED。

直接新建文件的七字段一致；先replace再观察、已无后续写入的稳定文件，path ctime_ns为1792158524161456600，open/read-end fd为1792158524163467700，其他dev/ino/mode/nlink/size/mtime_ns六项和固定payloadSHA均一致。原比较会在read-open拒绝。开句柄前替换负对照仍因ino/mtime/ctime变化拒绝。原probe没有保存birth原字段，因此不能说后续birth桥接或新比较流程已通过。

精确版本 [CPython3.12.10 fileutils.c](https://github.com/python/cpython/blob/v3.12.10/Python/fileutils.c#L1108) 的句柄stat使用ChangeTime；[posixmodule.c](https://github.com/python/cpython/blob/v3.12.10/Modules/posixmodule.c#L2139)的路径兼容分支将birthtime复制至ctime。这支持本次跨API字段语义不同的实测解释；[上游问题157671](https://github.com/python/cpython/issues/157671)记录了3.14的同类差异，不能代替本次3.12记录。

当前Python exe SHA `4d6f5f81a4bca11191c4c7c6b43632694d0a4ce74e068619d8fdc161d469859a` 与原01cd失败包保存python.bin相同；这只绑定exe，不证明完整DLL运行时，也不能补回旧A未记录的具体文件/字段。已确认一种稳定文件会被原比较误拒绝的机制，原make内层首错仍UNKNOWN。

统一取值修复现已实现：临时path handle先以六个同语义字段及双方实际birth绑定原regular/lstat批准对象，再以真实fstat完整七字段作snapshot与read/copy/reread/final比较。birth仅作额外桥接，不替换ctime；第二次路径检查使用fresh fstat，最后路径检查重开handle，缺birth直接拒绝。POSIX、目录、20秒/40MiB、首错及原运输闭集保持。

宿主反例由原稳定文件被拒绝变为成功，18项聚焦及完整transport模块55项通过；涵盖七字段各阶段变化、桥接字段与birth缺失、实际文件替换、同大小恢复mtime、句柄关闭和诊断失败。小probe复用固定源码中的真实Reader，保留旧raw观察并增加两次stable读取及实际read-open替换负控。以上是宿主合成语义测试，尚未证明Windows实际birth可得或新流程原生通过；后续普通Windows CI小probe验证后才决定是否再运行长A。

三源实现独立审查通过：5104B报告SHA256 `b0ece2dc17fec817fed666367f7112dd13039e9d515d04910ba220819b55516f`，root全文读并核38作者项、8保护输入及12审查绑定。普通Windows流程将运行固定无参probe，核对实际birth、稳定接受和read-open替换拒绝；长A工作流未改，未手动dispatch/rerun。
