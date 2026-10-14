# HEVC 16×16 NEON：Mac ARM64 私有正确性验证

2026-10-14，Task 4C5D1B。状态为**开发中**。两个 8-bit AArch64 NEON
planar/angular 内核的源码及 Mac ARM64 私有执行门通过独立 Spec/Quality 复审。
没有性能、完整 FFmpeg 配方、正式包或真实会话资格，生产默认和 codec 广告保持。

## 实现范围

`third_party/ffmpeg/8.1.2/patches/0002-aarch64-hevc-intra16-neon.patch` 只修改
五项 FFmpeg 源路径：新增两个 AArch64 文件，接入 pred.c/pred.h 和 AArch64
Makefile。仅选择 8-bit、NEON 的两个 16×16 context slots；其它尺寸、深度和
slot 保持。原 template 与 H.264 补丁不变。

本补丁尚未注册到组合 source recipe。Windows 在该私有 prototype 中有明确
不可用门，后续仍必须实现其 COFF/SEH 和原生资格；这不是取消 Windows ARM64
产品支持。Linux/OHOS 容器和所有正式构建资格分别待验证。

## 实际正确性与 ABI

使用已存在的只读 FFmpeg 8.1.2 配置树、原版 C include-TU 和冻结独立整数
模型，Apple clang 21.0.0 编译私有对象并链接原 dist avutil。没有第二次完整
FFmpeg 构建，也没有修改接受树。

- 实际 initializer 选择两个真实符号，depth 8/9/10/12/0/7/16、CPU off/on
  context 对照通过。
- 46 种图案、119600 组原 C/独立模型/NEON 完整 output/input 对照通过。
- 4800 组共用保护页 inventory 所对应的冻结执行路径通过；它未单独打印该
  计数，不累加到上述常规 cases。另有 1472 组 planar 最小只读范围测试通过。
- 73600 组直接 ABI/数值测试通过，包含 int 参数高32污染及寄存器、SP、LR
  检查；没有经 C 恢复桥接。
- 六类已编译负例精确拒绝：错误 slot、rounding、负索引取整、transpose、
  luma correction 和 tail write。每类 compile/link 成功，程序实际 exit 1，
  不能以信号、缺符号或任意错误替代数值判别。

首次真实 ASM 编译因 Mach-O 局部标签/CFI 失败。两个最小实测定位后，仅修改
标签前缀，完整汇编随后编译通过。另一次验证脚本误匹配错误文本的失败也已
保留，只补未完成项目，没有重跑已完成门。

## 确定性取整负例修复

独立执行复审发现旧负索引负例可能读到未初始化 ref[17]；虽然它实际编译和
精确退出，该常量图案不能作为稳定的 floor 数学证据。旧记录保持。

新的私有正/负配对共同初始化额外参考 byte，唯一算术差为 floor 与 truncation。
1056 条静态读取边界复核通过；新正例完整矩阵通过。实际非恒定 pattern6/
mode11/c0 的独立模型、原 C、错误变体分别为 **233/233/241**，输入和 padding
保持相同，两个错误变体观测均精确 exit 1。独立最终执行复审关闭该 P2。

## 禁用门与实际机器码

两个完整私有 header 配置分别禁用 NEON 或 HEVC decoder：真实预处理断言、
编译/链接、context/119600/共用 guards 通过；不链接新 ASM 仍闭合。该证据仅为
精确 feature bits 的私有 compile/link 门，不证明 configure/Makefile 或禁用包。

实际 linked Mach-O planar 核为 148 bytes，angular 核为 684 bytes；精确核
反汇编、object/linked 代码和三个 ADRP/ADD 指向的常量表均独立核对。所有实际
运行绑定原 dist `libavutil.60.26.102.dylib`，SHA-256
`29b18c1c99fe94fbb369191ab3b4602d671f0903019f3bf520a948c193c21ac6`。
冻结 source/config/compiler/复用对象/avutil 的 before/after 一致。

主要本地证据：`target/task4c5d1b-correctness-fix1-142b_qkc/`、
`task4c5d1b-controls-resume-d2m18omt/` 和 `task4c5d1b-paired-execution-9eo3hdis/`。
最后 paired 工具 session 86688 exit 0；results SHA-256
`e586b8e907a1d8bd22d666ce4d5c972364ae31b073baee184dad430dd56ca330`。
各旧失败目录继续保留，不以新结果改写历史。

Benchmark 调用为 0。固定三轮性能、其它容器、Windows 原生 ABI/SEH、D2、组合
配方与正式包、普通 HAP/向量 PC、完整 HEVC profile 和真实远程互操作均未完成。
本页不授予生产 D1 完成或自动启用资格。
