# Windows ARM64 私有保护页与 QPC 接口：源码和宿主验证

2026-10-14，Task 4C5C3B2A。实现状态为**开发中**。本次完成 Windows ARM64
私有数值测试接口的源码与 macOS 宿主算术/注入逻辑验证；没有取得真实 Windows
编译、原生执行或修改配方包资格。

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
