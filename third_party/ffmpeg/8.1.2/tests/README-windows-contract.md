# Windows ARM64 私有 ABI/SEH contract driver

日期：2026-10-14。状态：**源码冻结候选；未编译、未链接、未执行**。
`h264-luma-dc-windows-contract.c` 只准备原生 Windows ARM64 的 helper/outer ABI
和 SEH 正负控制。独立源码审查及 root 放行是执行前置条件。代码没有数值核、
数学 oracle、FFmpeg shipping 接线或性能资格；不会改共享 `cases=5974518`。

私有命令契约：

```text
abi-contract-N.exe --suite register|unwind|isolated|all --expect-variant N
```

N=0..4。每个 executable 编译一个 matching driver、直接链接一份 matching helper
和同一实际 `negative.obj`，没有重复同名 helper。未知 suite、bad5、错误配置参数、
非私有/非 Windows/非 ARM64/ARM64EC source target 都拒绝。0 是正确控制，1–4 是
明确坏元数据的控制 executable，其正常 ABI 返回仍必须通过；N 不是跳过开关。
返回 2 是 admission/参数拒绝，1 是 contract 失败，0 只表示指定 ABI 控制符合
预期。最终状态最多 `ABI-contract-only/nativehelper-control`，绝不是 full-qualified。

## 实际输入和原型

本次直接读取并保持只读的依据是 run `38516856053`、commit `122f44e` 的
`target/windows-private-coff-122f44e-38516856053/compiled-evidence/`，及
`task-4c5c3b2c-windows-compile-probe-review.md` 的独立 actual object 审阅。
固定工具为 llvm-mingw20250709 msvcrt ubuntu-22.04 x86_64，archive SHA256
`422f13915a41acd8b93566e590531f55d3de6e531575197d51756ff8e8d16525`；实际
Clang 20.1.8 / AArch64 Windows GNU。该 run 只证明此前 assembly/object 门。

| 不可变输入 | SHA256 |
| --- | --- |
| `h264-luma-dc-abi-windows.S` | `3568fa9d35929cf05b5c2e5b13e0dff7b840ffba23a6864c6e9bda34e50c22f3` |
| `h264-luma-dc-abi-windows-negative.S` | `a0f190242d9863ace83749cea918cb0cff91b7d3695d2ea45a7ffe91218b4db4` |
| `README-windows-abi.md` | `7486bc1a0f94dbf905e5144b0b5dff355af2e2a522cc03c4376864518801bbfc` |
| `abi.obj` | `8b2ce10ad66d89fb9267cc105a74669eaa1c2d928b644f1c122ed95748610b53` |
| `bad-unwind-1.obj` | `1b53a868f87faedfeafde9f4a13744c435e5c78f53a57bf20f93e9a7cec66dfa` |
| `bad-unwind-2.obj` | `dd15e7406fabb70207aed330ff55689e01348b76b79b78527278aaf7e98b5046` |
| `bad-unwind-3.obj` | `06ca2a927a06115351efa72e795c4eb3bee9eeccdbada37324966f000c9016d9` |
| `bad-unwind-4.obj` | `c1e0fc34723116bb6bf9f1aac8acf3e896f292a670fd17233614444e9d92aaf2` |
| `negative.obj` | `6ca74757ce0c57b7328b7a5e79e46fd93abb0039a8c603afecae420a861e63a1` |

现有 README 的精确类型为 `void kernel(int16_t*,int16_t*,int)`、
`int checker(int16_t*,int16_t*,int,kernel)` 和
`uint64_t outer(int16_t*,int16_t*,int,kernel,checker)`。
所有 register/structural probe 都只作为安全 outer 的 callback/checker 参数。
PC labels 声明为字节地址，从不直接按猜测的函数签名调用。
四参数 qmul wrapper 类型单独声明，仅保留接口边界；本驱动不调用它、不构造
fake 数学 kernel，也不把 preserve leaf 的 ABI 成功当作数值 parity。

实际固定 archive 的 header 正文由 root 提供到 readonly
`fixed-header-evidence/`：`winnt.h` 的 ARM64 `X[31]`、`V[n].Low`、
`Sp/Pc/V=0x100/0x108/0x110` 与 `RUNTIME_FUNCTION` 的 8-byte 类型；Rtl* 的
`ULONG_PTR/PULONG_PTR` 参数；`processthreadsapi.h` 的 ProcessMachineTypeInfo；
`wow64apiset.h`、`minwinbase.h`、`debugapi.h` 等的真实 native/debug 声明。
headers.json SHA `663f5b8518b3834879e6f9491ebc9417276869a81922a30962a9e47faa478168`，
headers-supplement.json SHA `248e45e2a6f249df935a365e63004f5e29daae0e0536b8406741f8ae2efe1e05`。
固定包没有 `jobapi2.h`；job APIs 使用真实 `winbase.h:2922–2929`，没有补造 SDK。
这不是 compiler 实际接受新 C 的证据。

原生 admission 使用 [IsWow64Process2](https://learn.microsoft.com/en-us/windows/win32/api/wow64apiset/nf-wow64apiset-iswow64process2)
确认 host ARM64 且没有 WOW emulation，使用
[GetProcessInformation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocessinformation)
和 [PROCESS_MACHINE_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/ns-processthreadsapi-process_machine_information)
明确 process ARM64。这两个查询由 kernel32 真实 export 获取；missing export 或查询
失败是 typed refusal。再核对自身 PE Machine=0xAA64/PE32+、CHPE metadata 为零、
64-bit pointer、实际 capture SP 16-byte 对齐。source target 宏不是 native proof。
ProcessMachineTypeInfo 需要 Windows 11（build22000+）；不放宽到不可查询的平台。

## 有限 ABI 控制

`register` 先 preserve 和全部 SIMD 高半区改变两个 mask=0 正例，然后 18 个
register × 低 bit0 / 低64内 bit40 两种实际 corruption，逐一精确比较低32的
单 bit、同时要求 outer 高32为零。另有18个 fake checker：低32严格零、outer
高32严格对应单 bit。x18/FP/SP snapshot 注入分别精确 bits18/19/20，真实 x18
从不被硬件写入。各 case 有 `_lo`、`_bit40`、`_fake_checker` 固定 ID。
若漏比较高32、误比较 SIMD 高半区、漏恢复 caller 或漏某个比较，都有对应
控制改变结果。没有掩码截断或“任意非零就通过”。

`isolated` 有六个固定 child：preserve 正控制、FP、SP+16、SP+8、错误 LR、
no-return。child 做同一 admission 后写入 exact LF ready 行并 flush；parent
读到 matching case/variant 后，发送 8-byte `{magic=0x46524431, case_id}` GO；
child 在读取 matching GO 之前不调用 probe。ready/result 协议用 WriteFile，
避免 Windows CRT text-mode CRLF 改写。普通诊断仍原样保存。

parent 用 [WaitForDebugEvent](https://learn.microsoft.com/en-us/windows/win32/api/debugapi/nf-debugapi-waitfordebugevent)
和 [EXCEPTION_DEBUG_INFO](https://learn.microsoft.com/en-us/windows/win32/api/minwinbase/ns-minwinbase-exception_debug_info)
采集实际异常。结构损坏必须是 helper 已绑定 RVA+0x408 的实际 `brk #0xf000`
(`0xd43e0000`)，错误 LR 必须是 wrong_return label+8 的 `brk #0xf000`
(`0xd43e0000`)。要求 Windows EXCEPTION_BREAKPOINT、精确异常 PC、ReadProcessMemory
读取的真实 instruction、一次 first chance 和一次 matching second chance，以及
同异常退出码。通过 [ContinueDebugEvent](https://learn.microsoft.com/en-us/windows/win32/api/debugapi/nf-debugapi-continuedebugevent)
把真实 probe 异常交还 OS；不修改 PC/SP、不 swallow、不从 trap 恢复成正常 PASS。
初始 loader debugger breakpoint 只允许 ready 前一个、在 child PE 之外；所有事件
仍记录。任何其它异常、ordinary exit、missing ready、probe 返回或 timeout 都失败。
实际 target 若报告其它 exception code，应保留失败，不静默扩大允许集。

三个 Windows trap 统一使用 documented `brk #0xf000`（`0xd43e0000`）；outer
frame failure 位于 outer+0x3f8，不是上述 child 的合格落点。C linked admission
和编译门分别核对三处实际字节。CREATE_PROCESS_DEBUG_EVENT 输出唯一
`FRD1 child_image case=... pid=... base=...`，base 来自 OS 的 lpBaseOfImage，
并在该事件的 ContinueDebugEvent 前记录。F1 使用调用方持有的 F2 已认证同 run
image_size、三函数 RVA/length 独立核对各 case 的 first/second/result PC；
不得从 trap PC 反推 base。共用 opcode 不取消 site 区分。该字段不是外部 child
模块文件或 provider 身份证明；编码修复是否消除额外 AV、通过原严格生命周期，
仍须新同 run 编译和原生结果证明。

no-return 是唯一指定超时：GO 后至少1500ms，没有 probe exception/result，parent
TerminateProcess，实际 EXIT_PROCESS_DEBUG_EVENT+wait terminal，并核对 forced code。
收到 exit event 只设置 `exit_event_seen`、code 和 event ticks，不能令 terminal=1。
之后保存 ContinueDebugEvent 的实际返回值及失败时即时 GetLastError；仅 continue
成功，且对独立 DuplicateHandle 持有的 process handle 等待得到 WAIT_OBJECT_0，
才设置 terminal=1 和确认时刻。WAIT_TIMEOUT/WAIT_FAILED/其它返回均保留 terminal=0，
其中 WAIT_FAILED 保存即时 GetLastError；continue 失败不执行 process wait。
各记录带 attempted/valid 标记，未调用时的默认字段不能被当成成功 API 结果。
状态为 expected-bounded-timeout/parent-forced，不能声称正常成功。所有 child 有10秒
watchdog；其它超时只是失败。job kill-on-close 和 debugger kill-on-exit 持有 child
所有权，正常路径确认终态才返回。debug API 丢失终态时 parent 输出 terminal=0 并
失败退出，触发 OS debugger/job 清理，不能继续下一 child 或形成成功记录。
所有 RIP_EVENT（包括 warning）永久失败并保存 case、PID/TID、event code、
dwError/dwType；未知 event code 也保存并永久失败。RIP 不是探针 exception。
CREATE_THREAD/EXIT_THREAD/UNLOAD_DLL/OUTPUT_DEBUG_STRING 是显式允许的普通事件，
会记录类型和 PID/TID，继续按 Windows 规则管理 debugger event handles。

保存 stdout/stderr 原始 bytes（各至多32767 bytes，溢出失败）、PID、ready、GO、
exception code/flags/PC/first-chance、真实 brk、exit、timeout、forced、terminal；
额外保存 exception parameters 和开始/ready/terminate/terminal 的 GetTickCount64。
exit event ticks 与确认 terminal ticks 分开；另保存 exit continue 和 process wait
的 attempted/result/error-valid/error 原始事实。任何 unconfirmed 先封存 raw facts，
再持有 job/debugger 所有权结束 parent，禁止启动下一 child。
原始 child bytes 用 WriteFile 原样封装，避免 parent CRT text-mode 二次改写。
Windows 没有 POSIX signal，记录 `signal=not-applicable-win32`。没有分离 child、
交互错误弹窗、隐藏 crash 或任意 timeout 通过。

## 实际 SEH 和独立上下文

使用真实 [RtlCaptureContext](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlcapturecontext)、
[RtlLookupFunctionEntry](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtllookupfunctionentry)
和 [RtlVirtualUnwind](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlvirtualunwind)。
helper 长0x440、实际 frame192。链接后的 helper VA 取真实符号，核对 labels 的
0/4/0x30/0x1a0/0x1a4/0x410/0x414/0x43c 相对偏移、实际 prologue/epilogue
instruction bytes、同一 `.pdata` 归属、full `.xdata` 长度和固定 variant bytes；
object-relative offset 从不当 RVA。outer 从独立 outer symbol 取VA：长0x430、
return 相对0x188、epilogue相对0x400，独立检查指令/table/xdata。
内部 EpiloguePacked 不会被误称为 packed RUNTIME_FUNCTION。

synthetic suite 为8个已知 PC 构造独立 frame/context。caller 值包含独特 full64
GPR、SIMD low64、FP/LR/SP/PC/x18。entry/after_alloc 的未保存槽为 poison；body
全部 saved slots 已写但 pattern 尚未装入；call/return 的 patterns 已装入；0x410
所有 callee regs 尚未恢复；0x414 的 d14/d15 在 context 已恢复；ret 所有寄存器/SP
已恢复。correct expected 从已审查实际指令/固定192-byte saved slots 形成，永不
从 Rtl 输出或待测 metadata 计算。expected 本身不交给 Rtl*，每次只传 actual copy。

| helper | 必须精确观测的差异 | 同 executable 正控制 |
| --- | --- | --- |
| 0 | 所有8PC full caller comparison=0 | actual capture walk +正常 ABI返回 |
| 1 | 所有PC lookup null，明确 missing-nonleaf-table；绝不 leaf unwind | 正常 ABI返回、outer 正确表 |
| 2 | after_alloc/body/call/return/0x410/0x414 只 SP 错，恰好 callerSP+16 | entry/ret 精确零差异 |
| 3 | body/call/return/0x410/0x414 只 x19/x20 错，分别等于独立 saved x20/x21 | entry/after_alloc/ret 零差异 |
| 4 | 0x410 只 d14/d15 low64 错，恰好保留 callee patterns | body 和0x414精确零差异、actual walk |

1–4 的控制成功表示已精确辨认坏 helper，不能标记这些 helper 为 native qualified。
正确0 baseline先运行是外部命令执行前置顺序，随后各坏 executable；一个坏 exe
不能单独授予 baseline qualification。bad5 的实际 MC 40 vs44 指令范围 exit1、无
object 只保留 compiler-rejected 证据，不链接、不列 native candidate。

正确 helper 与 body-correct bad4 还在实际 callback 中 capture，再 Rtl unwind
callback→helper→outer→C caller。callback 的 compiler frame 必须由真实 table
回溯；已知 x29 链定位 helper/outer frame，VirtualQuery 验证可读，按固定 saved
instructions 在任何 helper Rtl 调用前冻结独立 caller slots；helper 保存值还必须
等于已知 outer patterns。回溯核对 exact PC、SP、FP、LR、GPR全64、SIMD low64、
x18，以及各级 matching lookup/table ownership。编译明确保留 FP 并禁 sibling
calls；实际 C compiler frame和PE table仍需要独立 object/PE 审阅和 native观察。
坏1/2/3不继续其错误body unwind结果猜caller，而按精确synthetic负控制拒绝。

## 未执行的编译/原生门和证据格式

完整固定命令向量写在
`target/task4c5c3b2d-source-windows-contract/commands-after-review.sh.txt`。
仅用 `/opt/frd-llvm-mingw-20250709/bin/aarch64-w64-mingw32-clang`、真实 headers、
C11/O2/Wall/Wextra/Werror/FP/无siblingcalls，分别编译N=0..4；直接链接上述已产生
的exact helper和negative objects，使用默认 MSVCRT closure及kernel32/ntdll。
链接不包含 FFmpeg kernel/avutil，也不启动第二份生产FFmpeg build。最终实际
库归属/CRT/imports/-###/map的支持必须通过新真实compile/link receipts确认。
固定工具不接受源码或链接时保留失败，不能换工具或补造API定义。

后继需要由 root 在源码审阅后外部 pin 新 driver/README hashes，真实固定Linux
host编译/链接、保存每条 argv/stdout/stderr/exit/PID/wait_terminal、driver/object/PE/map
hash及readobj/objdump结果。受信来源是independent root输入，不是artifact自写清单。
同一source/runtime仍需按[官方Windows ARM64 runner](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
`windows-11-arm` 执行原生
host/process admission：先variant0 register/unwind/isolated，再variant1..4 unwind。
不得以Windows runner label、PowerShell进程或source宏替代exe自身的native admission。

最终PE绑定记录建议独立schema `freeremotedesk.windows.h264dc-abi-contract.v1`：

```text
source: driver_sha256, readme_sha256, helper_source_sha256, negative_source_sha256
compiler: archive_sha256, extracted_inventory_sha256, headers_manifest_sha256,
          driver_sha256, clang_version, compile_argv[], link_argv[], terminal_receipts[]
inputs: variant, helper_object_sha256, negative_object_sha256
pe: sha256, bytes, machine=0xaa64, optional_magic=0x20b, image_size,
    imports[{dll, symbols}], crt_dependency_hashes[], map_sha256,
    helper{rva,length=0x440,labels[{name,rva,offset}],pdata_rva,xdata_rva,bytes_sha256},
    outer{rva,length=0x430,return_offset=0x188,epilogue_offset=0x400,pdata_rva,xdata_rva},
    callback{rva,pdata_rva,actual_frame_and_unwind_review}
native: os_build, native_machine, wow_machine, process_machine, pointer_bits,
        sp_alignment, executable_sha256_before_and_after, suites[], child_receipts[]
status: source-only | compiled-only | ABI-contract-only/nativehelper-control | failed
remaining: shipping_kernel,qmul,full512_output,32_input,482_guard,QPC,
           shared5974518,pristineRED,parity,benchmark,package,live = OPEN
```

schema只是待root绑定runner的提案，不是假称已经实现新的私有build/native runner。
本驱动保留所有later gates：同shipping实际kernel对象、portable normalized/dequant
独立oracle、high32坏wrapper拒绝、完整512-byte输出/32-byte输入/482-byte合法guard
窗口、guard/QPC、共享5974518 fixture、nativePE/DLL/CRT/package/bench仍未实现或
未执行。当前未引入fake qmul或独立替代生产kernel来求绿。


## Failure-only bounded diagnostics v1（source/host，native 仍 OPEN）

`h264-luma-dc-windows-diagnostics.h` 是此私有 driver 的 header-only helper。
原 ASM、null-FP callback、三个 trap、normal stdout/F1、七条命令、异常
Continue/exit/wait 和原 10 s watchdog 不变。诊断 API 失败只锁存最终失败，
不接入提前 Terminate 的 infrastructure 分支；未知 AV 仍使原 isolated 合同失败。
正常成功路径不增加 stdout/stderr 记录。

父进程在每个非 loader 异常的停止点，以实际 event PID/TID 打开并独立关闭
`THREAD_GET_CONTEXT | THREAD_QUERY_LIMITED_INFORMATION` handle；核验实际
GetProcessIdOfThread/GetThreadId 后，读取 16 字节对齐的 ARM64
CONTROL/INTEGER/FLOATING_POINT context。原 ExceptionAddress 和实际 Pc 分别
保留，不调整任何寄存器，也不把二者相等当成先验。记录 PC/SP/FP/LR/PSTATE、
x0..x28、d8..d15 低64位。BOOL 的任何非零值均表示成功；失败即时保存 GetLastError，
失败 context 为 null，不能把清零缓存当 FP=0。

child 在收到 GO 后、outer 调用前，用 RtlCaptureContext 写入独立的
`frd_diag_reference` 全局记录（944 B，包含真实 caller context、magic、PID/TID、
case、complete）。固定 compile gate 从实际 PE symbol/section/初始 bytes 独立
认证其 RVA、16-byte 对齐、完整 writable/non-executable 数据范围。父进程只读
同一 child image 对应 RVA，并核验身份和完整 RPM。此步骤**不证明** caller
snapshot 到 outer call 期间 SP 不动，因此 H/O 仍为 `unknown-callsite-SP-proof`，
本增量不执行任何栈 dump。后续必须检查固定构建 child_main 中实际 capture→outer
callsite 的指令、控制流及 SP 调整，并结合两个192 B owned ASM frames，才可推导
H/O、增加各192 B独立有界读取。不得根据 case、bad FP、trap PC 或 observed SP
反推这些帧。

最多32个 CREATE_PROCESS/LOAD_DLL module 记录。其 range 来自 child 实际 image
header 的4096 B有界 RPM，保留 event base、PID/TID、load/unload序号。有效 hFile
立即 DuplicateHandle；原 event hFile 恰关闭一次，副本在 unload 或 cleanup 关闭。
缺 hFile、失效 range、多个匹配或已卸载均显式 unknown，不从父进程或 DLL 名称补值。
只对异常停止点 AV 所属唯一实际 module 的 hFile 做文件诊断：canonical path
上限1023 UTF16单元，前后 volume/file index/size/write-time，完整 BCrypt SHA256
上限32 MiB。所有 AV 共用1 s工作预算，逐64 KiB块检查；同步 Win32 调用本身的耗时上限及原
watchdog在真实主机上的相互作用仍需 native 验证。BCrypt 的 open/create/update/
finish/destroy/close 的尝试位和结果保留；hash只在完整覆盖且身份稳定时公开。
不归档完整 DLL。AV PC 周围至多256 B，记录 disk PE section映射算术、file offset、
实际 remote/disk bytes 与匹配结果。失败或部分 RPM 明示 API result/length，不补零。
读取和hash发生于 Continue之前，JSON缓存于每事件16 KiB buffer；退出后才输出，
从而保留首断点/AV/二次断点各自序号，避免读取已退出进程。

stderr 新块每行以 `FRDD1 ` 开头，后面为一个 JSON object；Windows CRT 的 CRLF
与 LF 均可读取。唯一顺序为 begin、0..32 module、0..8 event（AV紧跟对应 av）、end。
每 case最多一个块，总输出仍受原1 MiB stream限额约束。各记录字段由
`tools/verify-windows-private-diagnostics.py` 的精确键集校验；没有额外字段、重复键、
非整数布尔替代、NaN、无界raw或隐式默认值。

- begin：version=1、case、PID、声明事件/模块数、overflow、独立 reference RVA。
- module：index、event PID/TID、load/unload sequence、base/size/machine，remote
  header RPM、hFile duplicate、原/副本 close 的 API 结果。
- event：实际 seq/PID/TID/code/chance/ExceptionAddress、OpenThread、实际线程身份
  查询结果、GetThreadContext、CloseHandle、context或null、baseline RPM/identity/
  context或null、显式未知 frames。CPU context 与 exception PC 的不一致由 parser
  作为观察计数保留，无修正、无推导成功。
- av：对应 event身份、唯一 module index/base/RVA；或明确 module-or-file-unknown。
  有文件时包含路径/identity及API结果、hash阶段/状态/覆盖字节数、PE映射、code读取
  API/原始bytes。`api` 固定为 attempted/result/error_valid/error/got；NTSTATUS单独
  使用32位unsigned数值，不冒充 Win32 GetLastError。header/code/最后hash读各自分开。GetFinalPathNameByHandleW 的实际 DWORD 返回长度同时保留；仅返回0时 error_valid=1，非零但超缓冲由 path_valid=0 明示界限拒绝，绝不伪造 GetLastError。
- end：failed=1、diagnostic_failed、native=`unresolved`。原 `FRD1 failed` 行保留。

纯 parser 仅接受 stdin bytes，拒绝地址、文件路径或模式参数；其 exit0 仅表示该失败
诊断块结构/ownership/边界一致，输出始终 native=unresolved。它不接受 AV、不发
native成功凭据、不调用 Win32，不证明 FP→virtual unwind context→读取0x90 的数据流。
本观察增量仍需独立固定工具链编译/SDK/import检查、新一次same-run native失败或成功
证据，以及独立H/O证明；任何一项都不能从 host手写bytes测试推出。
