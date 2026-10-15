# Windows ARM64 私有 ABI/SEH 源码准备

2026-10-16 当前实现状态：**开发中；C0 私有失败路径已实现，待独立实现审查及新的固定工具链/原生门。**
下面 2026-10-14 的 source-only 及先前编译/失败记录是历史边界，不是当前源码的运行结论。
`FRD_DC_WINDOWS_ABI_CONTRACT` 开启时，调用前固定 ADRP+ADD x9 / CMP x3,x9 / B.EQ
绑定二指令 `corrupt_fp`，真实 BLR 在独占 32 B SP fragment 内执行。
只有这个精确、不改 SP/保存区的 callback 闭包授予 SP=H；不是对齐判断、case 字符串或可配置布尔值。
SP8/SP16 及其它回调保持主 helper 的 set_fp metadata 和原结构 guard。
`wrong_return` 原 16 B 四指令完全保留，另用独占 SP phantom metadata 描述继承的 H。
两个 raw records 各含 end_c、完整 192 B 保存布局及 end，不与主 helper/outer 共享 xdata。
variant1 同时省略两个 helper tables；variant2/3 在 helper SP fragment 同步错误 allocate/offset；
variant4 只改原主 epilogue 描述；wrong_return 的真实 H metadata 始终正确。原 bad5 精确拒绝门继续保留。

本机官方 SDK Clang15 仅做 changed-ASM raw COFF sanity，独立模型覆盖 SP fragment 的全部8条指令、
wrong_return 全部4条及明确元数据负控；generic macro-off 指令及正常 metadata 与旧 admitted object 逐字相同，
不含 negative callback relocation 或新 fragment。Clang15 实际接受 bad5，不能替代固定 LLVM20.1.8 的历史/后继拒绝。
当前没有新 pinned LLVM20 C/PE、Windows Rtl、isolated 或 shipping 资格。编译配方显式令私有 abi.obj 开启 C0；
generic checker 对任意坏 callee 的安全展开仍未关闭。ARM64EC/其它架构拒绝，所有产品 decoder/defaults 不变。
完整受审来源和本机输出见 `task-windows-failure-unwind-implementation-report.md`（SDD）。

日期：2026-10-14。状态：**源码提案，未编译、未链接、未运行**。这些文件只用于
Windows 原生 ARM64 私有控制台验证，不属于产品库、HAP、数值 GREEN 程序的
contract 扩展或跨平台资格声明。`h264-luma-dc-abi-windows.S` 是共享数值程序的
Windows checker 候选；negative 文件只能进入另一个私有 contract executable。
主代理独立审查和放行是实际编译/执行的前置条件。

## 官方依据与仍待观察的事实

已查阅 Microsoft 的 [ARM64 ABI 约定](https://learn.microsoft.com/en-us/cpp/build/arm64-windows-abi-conventions?view=msvc-170)、
[ARM64 exception handling](https://learn.microsoft.com/en-us/cpp/build/arm64-exception-handling?view=msvc-170)，以及
LLVM 官方 [llvmorg-20.1.8 AArch64 SEH assembler regression source](https://github.com/llvm/llvm-project/blob/llvmorg-20.1.8/llvm/test/MC/AArch64/seh.s)。
ABI 要求 x18 保留、x29 链式帧、16 字节 SP 对齐，x19–x28 全 64 位及
v8–v15 低 64 位保留；未使用的 int 参数高位不确定。Microsoft 的 unwind 格式
逐指令描述部分 prologue/epilogue。LLVM 官方测试提供 `.def/.seh_proc`、
`.seh_stackalloc/.seh_save_fplr/.seh_save_regp/.seh_save_fregp/.seh_set_fp` 和
epilogue 标记的候选语法依据。这里仅复用了这些基础编码，不使用 Windows 11
才支持的 `save_any_*`。

这些链接不是固定 llvm-mingw20250709 实际接受本源码的证明。固定 driver
`/opt/frd-llvm-mingw-20250709/bin/aarch64-w64-mingw32-clang` 在当前工作机缺失；
实际 LLVM 版本、COFF 输出、`.pdata/.xdata` 编码与目标 Windows 解释仍是 **OPEN**。
禁止换本机 clang、伪 compiler alias、另一个 llvm-mingw 或仿真执行填补资格。

## Checker 契约

```c
typedef void (*frd_dc_kernel)(int16_t *, int16_t *, int);
typedef int (*frd_dc_checker)(int16_t *, int16_t *, int, frd_dc_kernel);
int frd_h264_dc_check_abi(int16_t *, int16_t *, int, frd_dc_kernel);
uint64_t frd_h264_dc_outer_probe(int16_t *, int16_t *, int,
                                frd_dc_kernel, frd_dc_checker);
```

checker 和 outer 各使用 192 字节 frame：0/8 caller x29/x30；16–95
x19–x28；96–159 d8–d15；160 只读 x18 快照；168 expected SP；176
expected FP；184 scratch。x29=SP，指向真实 caller FP/LR pair。
每个真实保存/恢复/分配/建立 FP 指令都有对应基础 SEH 指令；快照属于 body。
外层常量使用另一组高低 64 位，外层也保存并恢复其 C caller。

checker 在实际 `blr x3` 前设置 10 个不同 full64 GPR pattern 和 8 个不同
low64 SIMD pattern。x0/x1 保留，w2 不变，x2 高位固定为 `0xa19c7e35`。
返回后的 SP 对齐和 x29=SP 检查先于任何栈读取。违反结构时 `brk #0xf000`
后永久 trap-loop，绝不借坏帧恢复。结构有效才比较快照与所有 pattern。

| bit | 可恢复 mismatch |
| --- | --- |
| 0–9 | x19–x28，完整 64 位 |
| 10–17 | d8–d15，低 64 位，不要求 vN 高 64 位 |
| 18 | x18 只读快照 |
| 19 | expected FP 与当前有效 x29 不同 |
| 20 | expected SP 与当前对齐 SP 不同 |

返回值是 OR bitmask，核的 void 返回值不参与判断。FP/SP 单独结构破坏可以
child trap 表示失败；bits19/20 的可返回路径用预期快照注入测试。只能从结构
一致性推断可以读取 frame，不能承诺对任意恶意 callback 同时伪造 SP/FP 或
整个保存区仍安全恢复。这里测试的是已知受控 ABI 破坏，不是 sandbox。

outer 的 uint64 返回值低32=checker mask，高32=caller restoration mask，
后者使用同一 bit 编号但独立常量/比较。外层结构失败 `brk #0xf000`。
四处 Windows trap 均为 documented `0xd43e0000`，由 case-specific 实际 PC
区分；共用指令字不允许把 outer failure 当作 child 的合格 probe。修正编码
本身不是 Windows 原生异常生命周期已通过的证据。
checker 参数允许用返回 0 却破坏单个寄存器的 fake checker，验证 outer 检测
确实独立；不是只信任内层“0”返回值。

## 已准备的探针与后继驱动必须判定的结果

negative 文件强制 `FRD_DC_WINDOWS_ABI_CONTRACT`，两文件拒绝非 Windows、
非 ARM64 和已知 ARM64EC 编译宏。所有 symbol 是 COFF 私有 executable 外部
链接名，不是产品 DLL export 或 FFmpeg 宏导出。

| symbol 或族 | 预期、判定边界 |
| --- | --- |
| `frd_h264_dc_preserve` | checker=0，outer=0 |
| `frd_h264_dc_simd_upper_only` | 改变全部 v8–v15 高64，checker=0，outer=0 |
| `frd_h264_dc_clobber_xN_lo/hi`，N=19..28 | checker 必须精确等于 `1u << (N-19)`；outer 高32必须0。hi 只翻 bit40，能发现 truncated GPR 比较。 |
| `frd_h264_dc_clobber_dN_lo/hi`，N=8..15 | checker 必须精确等于 `1u << (N+2)`；outer 高32必须0。hi 翻低64内的 bit40，能发现仅比较低32。 |
| `frd_h264_dc_fake_checker_xN/dN` | 用作 outer 的 checker 参数；低32必须0，高32必须是对应单bit，证明漏恢复能被独立检测。不得直接从 C 调用。 |
| `frd_h264_dc_fault_x18_snapshot` | 只翻 caller frame+160 快照；checker=`1u<<18`，outer高32=0。没有 x18 写入。 |
| `frd_h264_dc_fault_fp_snapshot/sp_snapshot` | 只翻 +176/+168 预期快照；checker bits19/20，outer高32=0，调用者状态恢复。 |
| `frd_h264_dc_corrupt_fp/sp16/sp8` | child 必须 ready 后异常失败；SP+16仍对齐但FP关系错，SP+8不对齐。不得把 ordinary exit1、缺 ready 或任意 timeout 算结构 trap 通过。 |
| `frd_h264_dc_wrong_return` | x30 跳到私有 controlled `brk #0xf000` landing；child 必须异常失败且无 PASS。 |
| `frd_h264_dc_no_return` | 专用 child 必须 ready 后超过明确 timeout，由 parent 终止并分类 expected timeout；不是正例成功。其它 probe timeout 是失败。 |
| `frd_h264_dc_call_qmul_hi_a/hi_b` | 四参数真实 tail-call wrapper，高32分别 `0xa19c7e35/0x5e6381ca`；同 w2、同实际 kernel，与独立 C 全输出及输入不变期望一致。 |
| `frd_h264_dc_call_qmul_bad_full_x2` | 将高32错误折入实际 qmul 后调用同 kernel；独立 oracle 必须拒绝至少一个明确 vector/qmul。不得只比较两次 wrapper 输出或只看 disassembly。 |

后继 qmul contract 必须覆盖正、负、0、-1、INT32_MIN、INT32_MAX，至少一个
可区分 bad wrapper 的输入。分别初始化并比较完整 512 字节输出（256 int16，
含240个 AC cells）、原输入 32 字节，以及 guard 场景的完整482字节合法窗口。
独立期望绑定共享 `h264-luma-dc-parity.c` 已有 `normalized/dequant` portable
semantics：uint32 modular multiply/add 与 signed floor 转换。后继root冻结适当
隔离接口，不在本source子步改共享C。INT32_MIN/MAX等极值不能以可能发生有符号
溢出的原upstream C表达式作为唯一期望；正常域原C对照仍由共享数值门保持。
不要从assembly pattern、modified NEON或高位污染自身猜数学。扩展不增加共享
`cases=5974518`或消耗共享rng。

## SEH 正负控制

同一个 helper 以 `FRD_DC_WINDOWS_ABI_BAD_UNWIND=0..5` 准备六个隔离副本：
0 正确；1 完全无 unwind 指令；2 实际192却编码208；3 实际x19/x20保存16
却编码24；4 实际恢复d14/d15却标为 `.seh_nop`，漏掉其 epilogue 恢复描述；
5 epilogue scope 晚一个指令开始但保留全套恢复描述，制造 scope/指令范围错位。
非零值必须同时定义 contract 宏。六份实际 helper 指令相同，都可正常 call/ret，
期望 object gate 拒绝非零副本；工具若在汇编阶段直接拒绝，记录该真实 rejection，
不得将未产生的副本伪称 native 负例。所有仍可产生的坏副本需要 native 已知 PC
控制证明普通返回不能替代 SEH。

公开的私有 PC labels：`frd_h264_dc_abi_pc_after_alloc`（sub 后、保存前）、
`..._pc_body`（完整 prologue 后、快照前）、`..._pc_call`（blr 指令）、
`..._pc_return`（blr continuation）、`..._pc_epilogue`（首个恢复）、
`..._pc_epilogue_after_d14`（第一对已恢复）、`..._pc_ret`（SP已释放、ret前）。
outer 提供 `frd_h264_dc_outer_pc_return`。labels 不制造新的 RUNTIME_FUNCTION。
其实际 RVA/指令边界待真实 object/exe 审查，不能预先声称是地址或绑定 proof。

native 门使用 [RtlCaptureContext](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlcapturecontext)
在真实 callback 内捕获，再以
[RtlLookupFunctionEntry](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtllookupfunctionentry) /
[RtlVirtualUnwind](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlvirtualunwind)
独立回溯 helper→outer→caller，
比较预先已知的 SP/LR/FP 和保存的完整 GPR/low64 SIMD。callback 自己若产生
frame，先依据其真实独立 unwind 回到 helper；不猜测编译器 frame。
对上述 prologue/body/epilogue PCs 另构造独立已知 frame/context，按照已审查
实际指令执行状态设定 expected registers/stack（包括部分保存/恢复），不读取
待测 metadata 来反推期望。body 用 SEH 正控制完整保存槽；after_alloc 的 caller
regs 尚在 context；epilogue_after_d14 的 d14/d15 已在 context；ret 前 SP 已为
entry SP、FP/LR均已恢复。逐 PC 验证 expected caller SP/LR/FP及适用保存寄存器。
缺 entry、偏移不符、任意参数/API失败、错误回溯都是失败，不能忽略。

## 命令准备与依赖 C driver 提案

未执行的真实固定 driver 命令清单在
`target/task4c5c3b2b-source-windows-abi/commands-after-review.sh.txt`。它仅准备
COFF assembly、header/symbol/relocation/unwind/disassembly 输出；不会启动第二次
FFmpeg build。选项依据 LLVM 官方
[llvm-readobj](https://llvm.org/docs/CommandGuide/llvm-readobj.html) 和
[llvm-objdump](https://llvm.org/docs/CommandGuide/llvm-objdump.html) 文档，固定版本
是否支持仍需实际 receipt。object/exe都应记录 SHA256。对正确副本检查完整 helper 范围、
192 frame、每一真实指令与 unwind、全部保存偏移和 epilogue，允许合法 packed
或 xdata，不能只搜索 `.seh_` 文字。ARM64EC、nonWindows、otherISA guard 的
拒绝也要分别记录，不把未知 target 的 compiler 报错冒充源码 guard 证明。

**依赖提案已告知主代理，未写入 C。** 下一单 writer 子任务需绑定新私有 C
driver 路径/API，承担以下工作：原生 PE/host/process 门；上述所有 symbol 的
显式声明/类型和期望 mask；隔离 child 的固定 case ID、ready flush、明确
timeout/真实异常退出码与无PASS判定；上述既有portable语义完整输出 qmul oracle；真实
Rtl* callback及已知PC正负context；绑定 root 认可的同 shipping kernel对象和
固定 toolchain artifact hashes。驱动不得把预期 crash恢复成正常 PASS路径，也
不得把特殊 no-return 的 expected-timeout规则扩散到其它负例。

候选未来 native CLI 契约是 `abi-contract.exe --case <固定白名单caseID>`：
parent native ARM64 process 逐个启动 child，记录stdout/stderr/exit/timeout；
`--suite register`、`--suite qmul`、`--suite unwind`、`--suite isolated` 分别
输出独立证据。实际参数、driver source和link receipts仍由后继root任务冻结，
本文件没有将不存在的 executable/CLI 宣称可运行。实际 kernel、avutil、CRT、
IsWow64Process2、异常码、known-PC构造、Rtl*行为全部仍是 OPEN。
