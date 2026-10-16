# HEVC 32×32 垂直 NEON 私有验证（2026-10-17）

本记录限于 macOS ARM64 的新 8-bit 32×32 垂直预测路径。正式配方、安装包、其它平台、性能和完整同流解码尚未验收，不改变 codec 广告或默认策略。

## 实现

0003 补丁在 0002 后新增独立汇编核及同 TU 方向 wrapper。mode18..34 使用 NEON；mode2..17 保留原水平 C 路径。真实初始化器仅在原 exact8+NEON、非 Windows 私有边界设置 slot3，其余位深和已有16槽保持。补丁 SHA-256 为 `bffa0d70b6dac644179dfc28b9e2978be17fdda68f8b7a1d8ea6b08117cba70a`。

## 已取得的实际结果

- 原版与独立模型118404 cases /236808 callbacks；候选完整selector、数值比较通过。
- 直接垂直核37536 cases；最小只读参考区150144 callbacks；直接ABI37536 cases通过。参考区使用保护页，目标缓冲区使用完整8192字节canary比较，不混称目标区保护页验证。
- 已核实际linked wrapper方向跳转、新核指令及ABI helper调用。CFI为静态记录检查，不表示异常展开或Windows SEH验证。
- rounding、index、direction、write-canary四个错误变体均被对应断言捕获，正常exit1；没有把编译失败当作负控成功。

原始结果分别保存在 `target/hevc32-neon-root-20261016/results.json`、`target/hevc32-negative-root-20261016/results.json`。正向及四负控独立审查见 `.superpowers/sdd/2026-10-16-hevc-angular32-vertical-neon/task-2-execution-review.md`。

## 最后一字节读取负控

原LLDB尝试超时，不能计为通过。新私有诊断仅对测试副本插入快照，将signal、PC、寄存器与case边界写入预打开文件；生产源码和模型不变。

首次启动exit206发生在诊断初始化，记录为空。最小复现确认Python对同一fd调用dup2不会清除原FD_CLOEXEC；新启动器显式set_inheritable(3, True)，保留失败原件。

同一冻结binary随后直接数值测试37536 cases、exit0且无异常记录；guards测试exit200，写出一条288字节记录。现场case为side1/shared0/id0/stride-index0/mode18/c0/which0/stride32；快照序号150146完整一致。实际Mach-O指令位于核入口+48，opcode `0x39407c2e`，即新增 `ldrb w14,[x1,#31]`；x1等于top，故障地址等于top+31和右保护页起点。实际signal为SIGBUS、code1（SDK名称BUS_ADRALN），不改写为code2。精确PC、字节访问地址和原PROT_NONE页共同构成归因，不能仅凭退出码认定通过。

证据：`target/hevc32-read-fault-root-20261016/runtime-inheritable-results.json`、`root-fault-binding.json`及288字节原记录。最后读取负控已通过独立执行审查，见 `target/hevc32-read-fault-execution-review-20261017/report.md`，报告SHA-256 `a9532363fdb96b4fe18fc961319c6b908e36c2b865d49d55ad71f7e26db90643`。该结论仅闭合本私有边界测试。
