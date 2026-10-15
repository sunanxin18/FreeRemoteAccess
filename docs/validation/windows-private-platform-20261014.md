# Windows ARM64 私有保护页与 QPC 接口：源码和宿主验证

实现状态为**开发中**。本文从2026-10-14 Task4C5C3B2A开始按时间记录。后续固定COFF/C-PE编译和静态产物验收已完成；2026-10-16严格日志解析器的源码/宿主控制也完成。真实Windows原生执行、私有ABI/SEH、同shipping数值与修改配方包资格仍未取得；各步骤证据和范围见下文。

## 实现与范围

`third_party/ffmpeg/8.1.2/tests/h264-luma-dc-platform-windows.c` 实现已冻结的
保护页与时钟接口：VirtualAlloc 保留三页、仅提交中间页，VirtualProtect 切换
只读/读写，VirtualFree 按原始保留地址释放；错误保留原调用与清理失败信息。
原生编译分支只允许 Windows ARM64，明确拒绝 ARM64EC 和其他架构。

QPC/QPF 使用整数换算、检查正频率及计数单调性，失败时不改输出或提交记录。
纯换算不使用浮点、128 位扩展或可能溢出的中间乘积。有限原始计数记录为后续
Windows 实测准备；本接口没有产品注册、构造器、main 或跨平台默认行为修改。

测试 TU 的 Win32 mock 只验证调用和错误处理，不能作为 Windows ABI 或系统
调用行为的证明。时钟算术是测试基础设施，不属于软件解码热路径。

## 实际宿主证据

独立源码复审发现并修复两项问题：矩阵实际总数为 10036，旧终止标记误报
10038；原 oracle 只核对数量，无法拒绝等量重复替换。修复保留全部输入与顺序，
oracle 独立核对完整输入序列、整数结果、终止标记和 EOF。

实际 `/usr/bin/clang -std=c11 -O2 -Wall -Wextra -Werror` 编译两份宿主
测试程序成功。执行完成：

- 22 项固定算术锚点和 10036 行完整输入序列，大整数 oracle 通过。
- 11 个明确拒绝负例，包括重复替换、缺失、乱序、错误结果及结束后额外输出；
  每项均为 oracle 明确 FAIL、实际 exit 1。
- 10 项独立进程的注入逻辑案例，覆盖保护页、QPF/QPC 失败、负数、倒退、
  相同计数、溢出和记录容量；每项实际 exit 0。

首轮记录脚本在短进程启动后查询 session 时发生竞态，实际 exit 1 已保留。
此前八项已完成案例没有重跑；独立续轮使用同一已冻结程序，只执行两项缺口并
保存 wait 终态。不能把首轮改写为一次完整成功。

原始材料位于本地 `target/private-windows-adapter-host/`：

- 首轮 `task4c5c3b2a-zaxj92r3/execution.json`。
- 续轮 `task4c5c3b2a-resume-ri5imir2/combined-summary.json`，SHA-256
  `79914c3c1a11b4c4c114cc1f7bd676ce158ef82f38ceea10a8e8857ad89f4e6e`。
- 73 项证据清单 `task4c5c3b2a-resume-ri5imir2/evidence-files.json`，SHA-256
  `c14663788de4668c626b6386184fc3c0890eee9eb85aedbc85db1169e109ebae`。

独立复审对源码和宿主执行分别给出 Spec/Quality PASS；root 复核七项源码和全部
73 项证据的当前 bytes/hash。没有重复运行已通过测试，也没有性能测量。

## 未完成门禁

固定 LLVM-MinGW ARM64 编译器路径在本机不存在，本轮没有 Windows 对象或
可执行文件。真实 Windows 保护页/QPC、ARM64 ABI/SEH/unwind、共享数值夹具
接线、同 shipping 对象的 parity 与基准、最终包及真实远程互操作均未完成。
其他架构资格独立处理；本私有 ARM64 接口不缩小 Windows 产品支持范围。

普通鸿蒙客户端的协议目录、codec 注册与真实 Windows/Mac 登录不受本次验证
影响，仍须按完整实施计划完成。

## Windows ABI 辅助源码准备

后续 Task 4C5C3B2B 的三个私有文件已完成源码准备并通过独立 StaticSpec/Quality
审查：`h264-luma-dc-abi-windows.S`、`h264-luma-dc-abi-windows-negative.S` 和
`README-windows-abi.md`。默认 helper 使用 192 字节有效帧链、明确保存与恢复、
逐指令 SEH 描述；独立外层探针覆盖调用者恢复、逐寄存器负例、合法 SIMD 上半区
变化、参数高位及隔离结构损坏。共享数值 C、原 POSIX ABI、recipe 和生产核保持。

这一结果仅为源码提案；没有 compiler、COFF、PE、native test 或 SEH 运行结果。
错误 unwind 副本须在预先选定的有效 PC 区分点与正确副本对照，不能要求所有 PC
都失败；若某个错误 scope 被汇编器拒绝，只能记录实际拒绝，不能计作原生负例。

独立 C driver、固定 LLVM-MinGW 编译、对象元数据与反汇编、真实 Rtl* 已知 PC
回溯、实际 kernel 的完整输出 oracle 及后续共享 parity/性能/包仍待完成。
INT32 极值期望须绑定既有 normalized/dequant 的无符号模运算及 signed floor
语义，不能用可能有 signed overflow 的原 C 极值结果替代独立期望。

## 固定工具链 COFF 编译验证准备

Task 4C5C3B2C 新增 `tools/compile-windows-private-abi.py`、对应窄测试和独立 CI
工作流。初次独立源码审查发现候选拒绝分类过宽：工具或输出 I/O 故障可能被当成
源候选拒绝；修复后未知非零默认保持失败，只有特定尾声长度诊断可记录为候选
拒绝。源码修复复审的 StaticSpec/Quality 均通过；这些测试尚未执行。

探针固定 Linux x86_64 构建主机、LLVM-MinGW 20250709 msvcrt archive 及既有
SHA-256，不使用本机 Clang 替代。十一项已批准私有源码/头文件按固定指纹检查，
普通 Windows C 使用真实工具链头文件。拟生成三个正确 ARM64 COFF 对象与五个
错误 unwind 候选，保留每次实际参数、终态、stderr、对象、readobj 与反汇编；
正确对象必须全部通过，候选未知错误不能变成成功。产物状态只允许 compiled-only
或 failed，不授予原生 ABI、SEH 或数值资格。

工作流保持手动触发，并仅在 `codex/rdp-egfx-hevc-preference` 分支修改该工作流
文件时触发首次运行。新增工作流尚未进入默认分支，故本轮通过独立审查后的明确
提交推送启动，不修改默认分支或既有包工作流。后续普通工具文件变更不会自动
重复此专项。运行使用 Ubuntu24.04，不是 Windows 原生执行。

截至本次源码提交，实际窄测试、下载/安装、编译及对象检查结果仍待取得。根代理
仅放行远端这一有限验证，保留首次实际失败后再修复。正常 Windows 包 CI 不能
替代它；成功 COFF 编译也不能替代后续 C driver、真实 Rtl* unwind、保护页/QPC、
同 shipping 核 parity、基准、修改配方包或远程登录。

## 实际 COFF 编译结果

后续冻结提交 `122f44e9a1c691a723cdb0229d7aa40b41ae123b` 的
[专项运行38516856053](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38516856053)
已成功结束。十九项窄检查实际通过；固定 archive 安装出的 clang20.1.8 在 Linux
x86_64 主机交叉编译 Windows ARM64：platform C、正确 ABI helper、negative ASM
三个必需对象均 exit0，bad-unwind1–4 也生成对象。bad5 实际 exit1，唯一诊断是
尾声实际40字节与 SEH 描述44字节不符，未生成对象；这是编译拒绝，不能算原生
负例成功。源码、工具、目标宏、真实头文件、七个 COFF 对象与全部原始检查路径
已由独立 ExecutionSpec/Quality 复审通过，范围严格为 compiled-only。

根代理下载 artifact11872184211，分别核对 installer30、compile130项严格成员及
字节指纹。compile ZIP SHA-256 为
`17c8ce32c22d39ef3e5f1bfc7c8b1bff1071e09c007316c1d283da5db8099732`；
原始材料位于本地 `target/windows-private-coff-122f44e-38516856053/`。正确 helper
实际 `.pdata/.xdata`、1088字节范围、192字节帧及 prologue/epilogue 与指令一致。
链接后的地址、真实 Rtl* 行为和调用者状态尚未验证，下一步为独立 C contract
驱动；保留旧源码准备文档作为该冻结输入的历史记录，不将当时的源码状态当作
本次实际编译结果。

这关闭了该有限编译子步骤，未关闭 Windows 原生 ABI/SEH、保护页/QPC、真实
shipping kernel/qmul oracle、数值、基准、修改配方包或鸿蒙真实登录等后续门。

## Windows 原生 ABI/SEH 驱动的源码准备

2026-10-14，新增私有 `h264-luma-dc-windows-contract.c` 与
`README-windows-contract.md`，按实际 COFF 指令和标签准备独立调用者上下文、
真实 Rtl* 回溯、寄存器正负控制及受父进程持有的异常子进程。初轮独立审查
发现两项问题：收到退出事件就记录已确认终态，以及静默忽略系统 RIP 调试
错误。修复后，同一审查者的 scoped StaticSpec/Quality 均通过：只有 exit
continue 成功且独立进程句柄等待得到 `WAIT_OBJECT_0` 才记录终态；RIP 和未知
事件保存原始字段并永久失败，未确认终态不启动下一个子进程。

该结论仅限源码。当前 C SHA-256 为
`e5ae7af8d9c7792bfecce5e876e92404331d6604820090dfc24212aecf7ccb6e`，
README SHA-256 为
`d67ea776ca6a5246f5ac0f8e0705d736e16bd5afdb7af5e7dc3f5bab4d3e44fb`。
独立 C/PE 编译接口已经完成源码与两轮修复复审：固定
五个 variant，使用同次工具链与 helper 对象，保存真实 C/PE、符号、导入、帧和
链接材料；全部接受检查完成后才原子发布唯一终态。后续实际运行见下节；
不以源码复审替代编译或原生验证，也不以 ABI 测试回调替代真实 shipping kernel
或 qmul 数学验证。

## C/PE 首次实际执行与导入检查失败

冻结 `f556b7b493d292a6e6c9b68c6a7776ed94246efc` 的
[专项运行38531352368](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38531352368)
实际失败。十九项原检查和二十五项新编排检查均通过，同次七个必需 COFF
对象生成；variant0 新 C 的真实编译、链接和 readobj 均 exit0。随后验证器
要求 PE 导入 NTDLL.dll 而拒绝实际产物，variants1–4 尚未运行。终态保持
failed；归档完整性通过只表示失败材料完整，不表示编译门通过。

实际 PE 只导入 KERNEL32.dll 与默认 msvcrt.dll，三个精确命名的
RtlCaptureContext、RtlLookupFunctionEntry、RtlVirtualUnwind 均由 KERNEL32
提供。这与 Microsoft 对这些 API 标明的 Kernel32.lib / Kernel32.dll 一致：
[RtlCaptureContext](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlcapturecontext)、
[RtlLookupFunctionEntry](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtllookupfunctionentry)、
[RtlVirtualUnwind](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlvirtualunwind)。
失败原因是验证器的导入假设，不是这次 C 编译或链接失败。修复只针对固定
配方的精确 DLL 集合与三个 API 的名称/提供者检查，保持编译参数、实际 SDK
声明和链接库不变；新执行结果须另行记录，不覆盖首次失败。

原始失败材料位于 `target/windows-contract-f556b7b-38531352368/`，artifact
11877688135。variant0 PE SHA-256 为
`1a0958de3ae27da9d00073e6d260f09f0abc2a96636ca503778d6f1a66e4316a`；
C 对象 SHA-256 为
`fd4e98406360e7fd14a189fb9567eba7a0f7f0c59e67ba964fbdd08430c569a0`。
尚未完成全部五个 variant 的编译接受，真实 Windows Rtl*、异常、等待、进程
清理和后续 shipping 数值/性能/修改包/真实登录仍未验证。

导入验证修复后的 29 项纯 Python 检查实际一次通过（含原 25 项和新增精确
导入控制），独立 scoped StaticSpec/Quality 复审通过。对未改变的首次 variant0
PE 进行纯字节 parser 观察也通过，未执行 PE，未补齐链接 ownership 检查。
首次 CI 的 76 个原始文件保持字节身份和 failed 终态。root 只放行新提交上的
同配方有限编译，仍不放行原生 ABI、异常、数值、基准或生产选择。

## 第二次 C/PE 执行与负例变体的导入差异

冻结 `65a22c583b6e0dd75c65a4aa58718aeb42c7284f` 的
[专项运行38542776507](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38542776507)
仍失败。19+29 项检查通过，variant0 完成 compiled-only 接受、绑定及有限
反汇编；其 `isolated_child` 被编译器内联，仍明确 OPEN。variant1 的 C 编译、
链接和 readobj 均 exit0，随后因未导入 RtlVirtualUnwind 被验证器拒绝。
variants2–4 未执行，没有原生 ABI/SEH 资格。

variant1 是缺少 nonleaf unwind table 的负例。固定 C 源在确认 Lookup 返回
NULL 后继续下一 PC，不调用 unwind_one；优化编译器因此删去 VirtualUnwind。
实际 variant1 导入仍为 KERNEL32/MSVCRT，但只包含命名 CaptureContext 和
LookupFunctionEntry。不能用假 C 调用或修改优化参数强制留下第三个导入。

第四轮 scoped 源码修复及独立复审通过：生产循环明确传入可信 variant index；
仅 variant1 不要求 VirtualUnwind，0/2/3/4 仍要求三个 API。三个名称的提供者
保护及精确 DLL 集合始终保留，不从文件名、receipt 或导入集合推断变体。
原 29 项测试保留，新增 7 项变体控制，共 36 项；本轮尚未执行。root 只放行
下一提交的托管窄检查及同配方 C/PE 编译，完整五变体和独立编译产物审查仍待完成。

artifact11883052555 的 API/local ZIP SHA-256 一致：
`6817be0b65d5de351252564015c3cb44510e1be5ecd885f619582f01a2f8dc5f`。
原始资料在 `target/windows-contract-65a22c5-38542776507/`。两个真实失败目录
合计 210 个文件前后指纹保持；第二次终态 SHA-256 为
`f030ef84e60c042197d593d903171afe80c542cb965088e9ac0c123071c9d936`，
状态仍 failed，不覆盖首次失败或将源码复审替代执行。

## 第三次 C/PE 编译及独立产物验收

2026-10-15 核对冻结 `30189bf67ee2e9743045275969cdd039a8cdba50` 的
[专项运行38548996801](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38548996801)。
实际 job115802047112 / attempt1 已成功。原始日志中的 19+36 项窄检查通过，
七个 COFF 对象与五个 C/PE 变体均完成；每个实际 C 编译、链接、object/PE
readobj 均 exit0。bad5 仍是预期 MC exit1 且没有对象，不当作原生负例成功。

独立审查从实际字节核对 18 份 source、固定工具 tar 的 8,123 项 inventory、
117 条工具调用记录及原始输出、42 段已保存反汇编的 13,995 个指令字，以及
36 条 owned full-xdata 绑定。五个实际 PE 均为 ARM64，导入闭包是精确
KERNEL32/MSVCRT；variant1 只有前述两个必需 Rtl 名称，其余四个都有三个。
summary 的 `packaging-pending` 不单独作为终态；独立终态为 `compiled-only`，
正确绑定最终 summary 和已核验的完整 archive。

artifact11885962816 为 80,618,176 bytes，API、本地 ZIP 和上传日志摘要一致：
`ddf55493a5be7f5832fe368bdf190a6fc294428a53ac641ba87aed1d6a9c8d7f`。
资料位于 `target/windows-contract-30189bf-38548996801/`，最终终态 SHA-256：
`fb83041aea5406bcc86c8ec1a97fd8a74a3dde870c967ddb3a56eccbdefd51e4`。
两个旧失败目录的全部 241 个文件保持不变，历史失败没有被覆盖。

传输有一项非阻塞限制：上传工具默认排除隐藏文件，外层 loose 目录遗漏
`inputs/.github/workflows/qualify-windows-private-abi.yml`；完整内层 ZIP 保留其
正确字节和摘要，因此本次以该完整 ZIP 验收，不能声称外层镜像逐项闭合。

本子步骤仅完成编译和静态产物验收。五个变体的 `isolated_child` 都没有独立
符号或 frame，内联边界保持 OPEN；真正 Windows ARM64 的 Rtl/ABI/SEH、
调试事件和独立进程退出确认、shipping 数值与保护页、性能和修改配方包仍未
执行。它们不构成鸿蒙客户端真实 Windows/Mac 登录、画面或输入的证据。


## Task4C5C3B2F1：严格日志解析器，源码/宿主控制完成（2026-10-16）

新增 `tools/verify-windows-private-transcript.py` 纯bytes接口及独立手写合成测试。它只接受variant0 register/unwind/isolated与variant1–4 unwind七个组合，解析原始长度分隔的child stdout/stderr，严格隔离parent/child命名空间，验证独立60项寄存器掩码、8个PC变体差值、六个child及exception→continue→exit→独立wait→terminal链。输入raw/line/stdout有界，未知/额外/重复/截断/字段错序、非零parent exit和stderr拒绝；不执行任何程序，不生成native-qualified receipt。

`parse_transcript(stdout, stderr, suite, variant, parent_returncode)` 中的退出码与字节均是调用方提供的数据，**不是实际Windows进程退出的证据**。本次全部控制为明确的synthetic host-only，最终22个测试方法及语法检查通过。测试包括七种合法组合、逐寄存器高低位、两字段一起伪造expected/actual、raw长度和嵌套final、异常机会/opcode、事件归属/continue/wait、指定1500ms强制终止以及失败/格式边界。没有重跑未改动的19/36编译测试，没有执行PE、修改C/ASM、重编译或发起CI/native job。 聚焦复现命令为 `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tools/tests/test_verify_windows_private_transcript.py -v`。

首次独立审查确认两项Important：原始Win32 BOOL被误固定为1、EXIT_PROCESS的TID被全局绑定最初主线程。冻结C与[ContinueDebugEvent官方合同](https://learn.microsoft.com/en-us/windows/win32/api/debugapi/nf-debugapi-continuedebugevent)仅要求BOOL非零，[进程调试事件官方语义](https://learn.microsoft.com/en-us/windows/win32/debug/debugging-events)将最终退出事件绑定最后退出线程。原实现的拒绝反例和日志保留。Fix1按规范signed32保存非零原始BOOL，并要求exit Continue与terminal摘要精确重复；六case取消全局exit==main/probe约束，继续核对当前PID、正TID、相邻Continue精确配对、独立wait及终态，受控主线程probe关系保留。新增合法非1/负BOOL/边界与不同last-thread正例，另保留零返回/越界/不规范数字/摘要不一致/单处TID破坏的负例。原17项变为最终22项全部通过；原审查五个合成控制由拒绝变为接受。首次RED、实现raw framing错误、自审与Fix1失败均单独保留。

同一独立reviewer完整复审Fix1后 **Spec PASS / Quality APPROVE**，I1/I2均关闭，无新增Critical/Important/Minor。root核对11项Fix1材料、6个原C/ASM/header pins、旧17份归档、8份旧证据和报告前10909bytes不变，完整阅读两轮报告与全部源码diff；没有重复测试代替审查。最终parser SHA256为 `c7aa44665a765679006174267be34b27f4dcb4fabe5fa84a3103815b45f918db`，test SHA256为 `306c522dbe3df4176dc40c0d31949ea9dd11744ffc78be4bb33555b442951429`；Fix1 manifest `ffc40d9f53ec1e00e12837ad5ced47624851b07ff7fa141c92f28e098f50bae0`，复审报告 `4165348cfabbb6de5aa558025c96198880173d10587a5ffc5ed0b6c532baf3ce`。证据分别为 `target/windows-private-transcript-20261016/` 与 `target/windows-private-transcript-root-20261016/`。

### 本门采用的有限oracle与后续原生边界

本门接受已审阅C内部独立saved-slot/context oracle，外部只核对实际发出的精确掩码/PC-ID/顺序/生命周期及未来真实parent exit0。C没有发出完整actual/expected contexts、child加载基址/模块身份或loader来源；parser不从trap PC减RVA循环推导基址，也不把matched当外部完整寄存器重建。初始pre-ready breakpoint只核对C的分类一致性，未独立确认loader身份。no_return计时是ready→terminate至少1500ms且go=1，没有GO时间戳。

只读后继预检确认 `isolated_child` 是parent debugger loop，源码内联不参与callback→helper→outer的真实Rtl链；因此取消“该源函数必须有单独frame”的前置要求，不改O2/工具链/recipe或强加noinline。capture_callback/helper/outer实际frame、已编译PC和保存槽依然是原生门。本裁定取代前节单独frame的OPEN前置，**不等于任何compiler frame已原生通过**。

本节当时尚未实现的后继包括可信host/PE/artifact admission、真实父进程wait与进程树所有权/清理、Windows ARM64 Rtl/ABI/SEH/debugger七命令及同run CI接线。后续静态验收模块见下一节；QPC、完整寄存器外部重建、系统DLL/API提供者身份、同shipping数值/guard/性能/修改包和产品互操作仍保持各自边界，22项合成日志控制不关闭它们。

## 同 run 静态产物验收模块（2026-10-16，B2F2）

状态：**开发中**。`tools/admit-windows-private-artifact.py` 提供 `admit_artifact(outer_zip, checkout_root, expected, metadata, destination)` 和 `revalidate(seal)`。只返回数据型 `admitted-static-only` seal；不联网、不认证调用者提供的API信息、不执行PE或编译工具。未来可信runner负责独立取得GitHub上下文与artifact元数据，保存返回seal的独立身份，并在每次使用前重新核对。

新transport精确包含本run/attempt的三个目录：installer和object各自的evidence.zip/archive.json，contract的evidence.zip/archive.json/terminal-status.json，共七文件。未来upload名称固定为 `task4c5c3b2f2-input-{run}-{attempt}`；当前workflow尚未接入该upload。保留完整inner源码及隐藏workflow，不复用旧combined外层不完整loose镜像。来源SHA/run/attempt、固定repository ID、artifact ID/name/size/digest与独立caller context/API数据逐项绑定。

落盘前检查ZIP规范路径、Windows设备名/别名、成员类型/重复/CRC/长度、精确成员集、严格JSON与全部摘要；installer/outer上限96MiB、JSON8MiB、合并展开256MiB。各inner archive绑定manifest、源码副本及当前checkout；contract必须有唯一compiled-only terminal，准确绑定已有packaging-pending summary。随后使用固定源码中的静态检查重算七个COFF、五个AA64/PE32+、variant1导入区别、helper/outer指令归属、pdata/xdata与依赖闭包。platform.obj由实际source对象和same-run objects绑定，producer linker completed-identities没有消费它，不伪造该记录。

seal发布前将全部落盘文件与原始内存bytes比较，保留完整树及源码/outer身份。失败保留首因和partial，不发布成功seal、不复用目录；`revalidate`检查caller持有的seal、源码、outer、全部输出与额外/别名文件。调用者不能从可写目录重新学习期望值作为权限依据。

初次独立审查发现 **I1：源码pin正确仍可能通过SourceFileLoader读取未校验pyc**，同时影响compiler与nested support。审查者在源码pin全部不变时实际复现替代缓存执行。原作者分别加入完整18源文件checkout的compiler/support缓存控制：旧实现两项均RED；修复为直接编译执行通过pin的同一份源码bytes，避开两个缓存loader，两项均GREEN。固定producer、C/ASM、现有缓存及workflow均未修改；原失败材料保留。同一reviewer复审为Spec PASS / Quality Approved，无未关闭C/I/M。

| 最终检查 | 结果与范围 |
|---|---|
| 新可移植定向suite | 22 discovered，21 PASS / 1 optional SKIP，不依赖本机target缓存 |
| 两条源码缓存替代控制 | 实际RED两失败→GREEN两通过；合法源码snapshot仍完整正确，缓存未删除或改写 |
| 显式历史产物静态回归 | 22 PASS；完整tar清单、七COFF/五PE身份、seal重验、PE变更拒绝及first-failure/no-seal |
| 两文件语法 | 通过；最终日志无warning |

显式静态夹具读取原 `30189bf` / run38548996801 / attempt1 的80,618,176字节外层ZIP，SHA256 `ddf55493a5be7f5832fe368bdf190a6fc294428a53ac641ba87aed1d6a9c8d7f`。七原始成员字节重新组成的transport、artifact ID1及元数据都明确标注为**合成宿主控制**；不是新同run CI产物、API实际观察或Windows执行。root重新核对七成员完整bytes与原件相等，原producer/pins及旧材料未变；本轮未运行旧19/36编译suite。

最终验收源码SHA256 `d1b98c9fb15be28ce77200b5eaba56b50750c0672f5d4b6e4513f80f8410cc48`，测试 `df463a29330930f503c83baded60b9b09c868c179ef79c0cd68fe16a399c1c66`。作者证据 `target/windows-private-admission-20261016/`，Fix1冻结清单1010项（SHA256 `f3fc61793b7e3290bdbf33c984010426d4750a5a3d9c35fdfdc0f15e41d39be2`）；root核对所有新项、3514个旧target证据及原报告/审查前缀，接收材料在 `target/windows-private-admission-root-20261016/`。独立复审 `task-4c5c3b2f2-artifact-admission-review.md` 位于本计划SDD目录。

下一阶段须完整接入Win32进程所有权、实时host/machine、七命令、实际wait/exit、有界tree清理、可信API下载、同workflow新upload与新run编译。workflow改动需要同步更新本模块的固定workflow pin并独立审查；不能放宽源码比较来沿用旧夹具。Windows路径实际运行、原生ABI/SEH、shipping数学/guard/QPC/性能/修改包及产品资格仍OPEN。本轮未观察或操作PC表单，也没有设备、HAP、账号或协议会话操作。

## 同 run 原生验证工具：源码与宿主控制（2026-10-16，B2F3）

状态：**开发中**。新增 `tools/run-windows-private-contract.py` 与 `tools/windows-private-process.py`，接入既有静态seal和bytes解析器。专项workflow增加精确七文件input上传、artifact ID/digest传递和依赖编译成功的 `windows-11-arm` job；原Linux编译配方、C/ASM、解析器及工具链不变。静态验收模块只同步workflow字节摘要，旧合成transport不能用于新workflow的原生验收。

Win32实现使用固定宽度ctypes结构；实时核对Python原始PE、IsWow64Process2、ProcessMachineTypeInfo和本机系统架构。每条命令通过STARTUPINFOEX的JOB_LIST在创建时关联独立kill-on-close Job，只继承三个标准I/O句柄；父进程保持挂起直到独立机器检查通过。固定顺序为variant0的register/unwind/isolated和variant1–4的unwind；非isolated30秒、isolated120秒、清理10秒，stdout/stderr各1MiB。接受结果需要父进程独立wait、exit0、Job活动进程数为0和严格日志解析全部通过，首错后余项不启动。

调用者使用同run/attempt上下文、编译job结果、固定repository ID、上传ID/digest与认证Actions API比对。下载禁止自动跳转和向存储域转发Authorization，保存原始ZIP bytes并核对API大小、摘要及源码身份；子进程只有公开系统路径的最小环境。每项执行前后重验caller持有seal、源码、PE与既有材料。封装后独立重读ZIP并再次重验输入，最后原子发布唯一terminal；QPC原整数只作为执行/超时记录，不是性能测量。

初次独立审查发现三项Important，均保留实际宿主反例并经同reviewer复审关闭：

- 固定上传action输出裸64hex，而入口错误要求 `sha256:`；现在只在action-output边界严格验证并规范化，分别保留原始值与REST/F2值，metadata不一致仍拒绝。
- HTTP200提前EOF时丢失已下载前缀；现在沿实际main失败路径保存有界partial。测试核对失败目录、members及ZIP里的10字节原文/摘要、首错和七项not_attempted。
- 退出后drain超限或第二流读错会丢失已读取的上限内字节；现在两处读取共用逐流保存逻辑。测试核对完整1MiB前缀、第二流失败仍保留第一流、清理状态与后续命令停止。

原候选owner15、runner19、静态验收21pass/1可选skip及解析器22项，共77pass/1skip。Fix1只复跑受影响runner22和owner17，共39pass；语法/pins检查通过，F1/F2未重复跑整组。最终日志无warning。root核对初始50项、Fix1新增28项、未改45项及两个报告原前缀；旧失败材料和源副本保留。审查最终Spec PASS / Quality Approved，无未关闭Critical/Important/Minor。

修后runner SHA256 `0db22e25ce7074fb8493088f7f0a0105568590ccdd9d6414b84e6fa82e7563d0`，owner `447dbba607851e835f980a7c6651355433a75f86df5c4718b711b8e7944151ed`；workflow `c3cf5d6948ba14647f0b9d65352020bdfc7f704d48e1f90d4a520df55dbd56b2`，F2 `6e08a333aa62e1775156696f721ec900bf352d192e76369bdd2870e6c2f2be15`。作者材料位于 `target/windows-private-runner-20261016/`，root接收位于 `target/windows-private-runner-root-20261016/`；复审报告为计划SDD目录中的 `task-4c5c3b2f3-runner-review.md`，SHA256 `29cecd9f229e7c74703e1b1943e4931ae841bf22d4c9f9d72a12c8239d53267a`。

以上只完成源码、宿主控制和CI接线。首次实际Windows ARM64 Python/ctypes/API、JOB_LIST/nested jobs、C debugger/child、pipe/wait/jobzero/cleanup、认证API下载及最终上传仍待验收。即使后续通过，范围也仅为 `ABI-contract-only/nativehelper-control`；外部完整contexts、child base/provider/loader身份、shipping数学/qmul/guard、解码性能、修改FFmpeg包及产品互操作仍OPEN。此次没有设备、PC表单、HAP、凭据或远程会话操作。

### 首次真实 CI：源码摘要拒绝，原生检查未尝试

提交 `8e2a3e8f4b97a06b4cce46e9142dcca2434d134b` 的[原run38785317016](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38785317016) / attempt1 已失败。Linux job116595688739的宿主控制、固定工具链安装、七COFF/五PE编译及精确七文件input上传全部成功；Windows job116599130157在 `source-pins` 阶段拒绝 `tools/verify-windows-private-transcript.py`。未执行launcher原生API admission，七条命令全部 `not_attempted`，没有PE/ABI/SEH结果。

root从认证artifact API取得native失败包11997442880，4849 bytes，SHA256 `d7991431b5356e2110fef811b22eb66e7d6ce87fbc116dfea5db6fe37bdb377b`；核对外层精确六文件、内层evidence.zip、members/摘要及terminal→archive/summary绑定，首错一致且终态failed。input artifact11998610962为78614325 bytes，API digest `sha256:892b62e7c4c67a192483d8baf8ad48da1f86c59b6912915068d416a20b4b7d99`。root在Mac下载该大包达到原60秒上限，未运行F2静态验收；另一次job日志GET失败，均作为收集限制保留，不能算成Windows产品失败或放宽下载上限的依据。未重跑原run。

旧入口在拒绝前没有落盘实际source-identities，故未观察到Windows失配文件的字节/摘要。现有证据支持CRLF检出推断：[Git属性规则](https://git-scm.com/docs/gitattributes#_eol)明确，`text=auto` 且eol未指定的文件仍受 `core.eol` 影响，[core.eol默认native](https://git-scm.com/docs/git-config#Documentation/git-config.txt-coreeol)；仅 `core.autocrlf=false` 不等于强制LF。root只读Git filter控制在显式crlf时生成18728 bytes/361个CRLF/SHA256 `6b9da36ae6c28af0faef800157e0cb0535a8df19da8bb8b6bb66eb0286264d6c`，指定lf则精确恢复Git原18367 bytes/c7aa4466。该控制不是实际Windows文件证明。下一修复将只为CI checkout增加过程级 `core.eol=lf` 并提前保存actual/expected source身份，继续严格拒绝失配，不在落盘后重写源文件。

原始metadata、native rawZIP/静态核验与收集限制位于 `target/windows-private-runner-root-20261016/first-ci/`。首次失败及全部宿主反例保留；实际Windows machine/API、运行与生产资格继续OPEN。没有观察或操作PC模拟器。

### Fix2：检出配置与拒绝前身份材料

native checkout过程环境同时指定 `core.autocrlf=false` 和 `core.eol=lf`；其余动作、Linux producer、运行/下载预算和属性文件不变，不做检出后的源文件改写。workflow摘要同步到F2唯一literal，再同步runner的F2 pin。runner在固定pin检查之前保存actual source-identities及独立expected-pins；仍按原失配代码拒绝，禁止加载未通过的私有模块或启动网络/PE。

新增真实临时Git控制读取实际workflow环境，证明旧单key产生CRLF、双key精确恢复原LF字节；actual-main失配控制核对actual/expected JSON已经进入failed ZIP、同首错、七项未尝试，且native/network边界未调用。两个反例先RED后GREEN；runner24项通过，受影响F2 portable21pass/1可选skip，合计45pass/1skip。owner/F1及旧compiler整套未重跑。语法/YAML/pin检查通过，辅助AST检查器第一次读取Name失败的原日志保留。

root核对Fix2的28项冻结材料、68项旧作者证据、12项首CI证据及15480-byte原作者报告前缀；同reviewer定向复审Spec PASS / Quality Approved，无未关闭Critical/Important/Minor。原审查15898-byte前缀保持；当前review SHA256 `3b47880adb2a181a92bc2f6f6fb5a91b19fd796a012b05117b9e22812b7ff2e3`。workflow SHA256 `3a2318bf94819b5c34c5aec0084441c33da997cf07393218d14e81a15d9429c2`，F2 `2463ef67c7f7bca0ec33b6f22c5e0def64497584d22bb226a216adda2f04ecea`，runner `66314d1cdaeaf64994b5205aed29b3a30aeba7caeba3687327cc8a96cc273f38`；源副本与RED/GREEN位于 `target/windows-private-runner-20261016/fix2/`。

Fix2仅是源码与宿主修复。实际Windows是否恢复精确字节、launcher machine/API、同轮artifact/F2、七命令和上传交付须在新commit的新run中验收；旧失配hash仍未知，不能用host控制补写首CI事实。首轮不重跑，原生和shipping门继续OPEN。


### Fix2 原生结果：一项寄存器通过，Job 收束检查提前拒绝

提交 `ff3042552b0b6a85945cd33ad08201b649dff02c` 的[原run38788965972](https://github.com/sunanxin18/FreeRemoteAccess/actions/runs/38788965972) / attempt1 已失败。Linux编译job116608227927成功；Windows job116610218496的源码摘要门通过，实际Python3.13.16为AA64/PE32+，Windows build26200，IsWow64Process2、ProcessMachineTypeInfo及本机架构检查通过。实际运行结果只有以下有限范围：

- `register/variant0`：PID2508/TID11012，独立wait0、真实GetExitCodeProcess返回exit0、JobActiveProcesses0，正常收束且未Terminate。root用固定F1重新解析实际原始输出，结果与运行记录一致。
- `unwind/variant0`：PID11008/TID8636，wait0后第一次Job活动计数为1。旧owner立即报 `descendants-still-active` 并TerminateJob；随后cleanup wait0/active0。没有调用GetExitCodeProcess，故真实退出码未知，不能用C stdout成功文案补写exit0。
- `isolated/variant0` 和 `unwind/variant1–4`：五项均未尝试。整轮仍失败，不授予完整nativehelper或shipping资格。

native artifact12000490061共472060983 bytes，SHA256 `b2650eea8ac114024d54e03d6aaa2bd8e9440d17959b288cf51dcb5895ba960d`，与认证API一致。root核对531个外层和528个内层成员的CRC/长度/摘要、terminal/archive/manifest/summary及七份源副本与Git对象。初次600秒收集停止及336789504-byte前缀原样保留；随后按同一artifact的固定offset只续传余下135271479 bytes，完整摘要确认后又将原Range诊断字节与完整ZIP逐项比较。没有重跑CI或原生命令。

同run input artifact11998669846共78612007 bytes，SHA256 `b9728722137ab2a3688b7eac881f1b884d892f276cf8ca263d11a1ec3ed7a6d3`。完整native包内保留其原ZIP；root比对独立认证API、caller context与实际字节，再用未改F2直接静态admit/revalidate通过七COFF/五PE。该seal仍为 `admitted-static-only`，root没有在Mac执行PE或编译器。接收记录位于 `target/windows-private-runner-root-20261016/fix2-ci/native-root-acceptance.json` 与 `root-static-acceptance.json`。

C的unwind分支仅执行synthetic和live-walk，没有显式CreateProcess；CreateProcess位于isolated路径。这不识别实际Job计数1的进程，也不证明无干预时是否会清空。[Job计数合同](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_accounting_information)与[进程终止合同](https://learn.microsoft.com/en-us/windows/win32/procthread/terminating-a-process)没有承诺parent wait0后的下一次Job查询必须为0；本记录只在Terminate之后观察到0，因此不能将OS计数滞后写成已证实根因。

Fix3的最小裁定是：在原30/120秒执行期限内等待parent wait0、真实exit0、Job活动计数0及有界输出读取共同完成；单次正计数表示待收束，持续非零到原期限仍失败，再执行原10秒清理。实际非零exit/API/读取错误保持立即失败，Terminate后的零不升级为成功，不启动后续命令。成本是真实残留Job最多占用原正常预算；不增加宽限期、不重试或改变JOB_LIST所有权。源码与宿主修复已完成并通过下述独立复审，完整七命令原生验证、shipping数学/guard/性能/修改包仍OPEN。

专项workflow的push过滤只匹配workflow自身。Fix3保持workflow/F2字节不变；独立复审并推送后，root应对精确feature ref启动一次新workflow_dispatch并绑定实际source SHA/run/attempt，不重跑旧失败run，不修改注释制造触发。此次没有观察或操作PC模拟器、HAP、凭据或远程会话。


### Fix3：原期限内联合收束，源码与宿主修复通过

owner在独立wait0之后实际查询一次退出码，要求DWORD整数0；非0、未知、API错误立即失败。正Job活动计数继续在同一原执行期限内逐流收取有界bytes并查询；只有wait0、实际exit0、JobActive0和drain完成共同成立才接受。每轮入口、查询后及drain后检查同一deadline，持续输出和计数波动不能延时；期限到则永久失败，再以原10秒cleanup确认，不因Terminate后清空而升级或继续下一项。

正常材料新增逐条wait/actual active/actual exit/raw QPC观察；首次 `normal_active_processes` 保留初值1，后续归零另行记录。查询尚未发生或失败保持未知。原JOB_LIST所有权、挂起机器检查、原始输出上限及逐流prefix保存路径保持；runner产品源码只更新owner摘要。

旧owner对人为1→0轨迹先RED，修复后GREEN。最终owner24项和runner26项合计50项通过；包括持续active的30/120秒模型、cleanup未确认、非0/未知exit、查询/读取错误、连续输出与计数波动不重置deadline，以及真实collect→orchestrate→固定F1 parser的七份合成日志。负向集成仅尝试首项，后六项未尝试。中途粗模型时钟导致一次断言失败的原日志保留；仅调整测试时钟采样粒度，产品预算未改。F1/F2/旧compiler suite未重跑。这些均为宿主控制，没有Windows原生或性能资格。

同reviewer定向复审 **Spec PASS / Quality Approved**，无Critical/Important/Minor。root核对37项新冻结文件、99项旧作者证据、8份基线、13个producer pins、固定F1/F2/workflow与22491-byte旧报告前缀；报告和完整四文件diff已读。最终owner SHA256 `efe57549a64a3c7418002c97d81ddd9af293b86e4ebaf824bc0a88081d2f5fb0`，runner `b569117d7d75139d1a8d0337ee483aa4aba659d08d165636595468de72b540ba`；Fix3 manifest `fb1dc182c50f1d109b086d73c401a2b18929f842456640243217886b3747b9d6`，复审报告 `2785a1133f6825773ab463737c32af39e4e4a3663dfc087fe75f8a4b3827f818`。作者材料位于 `target/windows-private-runner-20261016/fix3/`，root接收为 `target/windows-private-runner-root-20261016/f3-fix3-root-acceptance.json`。

原unwind actualexit、active1的身份和无干预是否自然收束仍未知。新原生运行须以审查后的精确新commit单独取得同run编译、输入及完整七项结果；旧run保持失败。完整helper、shipping数学/guard/性能/修改包和产品互操作仍OPEN。没有操作PC表单或其他设备。
