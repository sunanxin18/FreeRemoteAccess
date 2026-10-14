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
