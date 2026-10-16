# HEVC Angular32 Vertical NEON Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成已批准 Task4C5 的 8-bit 32×32 垂直 angular NEON 候选，保留原水平分支。

**Architecture:** 在现0002 postimage上新增0003补丁。真实pred_angular[3]指向无可变全局fallback的C方向wrapper：mode18..34进入新AArch64汇编，mode2..17调用同TU原pred_angular_3_8。仅exact bit_depth8、真实NEON及现非Windows私有边界启用；原32 C函数体与既有16槽保持。原版TU/独立floor模型为测试oracle，不是生产后备。

**Tech Stack:** 固定FFmpeg8.1.2、AArch64 NEON `.S`、C17、现有私有ABI/guard工具；macOS仅ARM64。

**Spec:** [已批准主计划Task4C5](2026-10-13-harmony-clients.md)、AGENTS.md多架构规则；细化仅实现既定缺口，不更改H264优先和HEVC后续验证顺序。

## Global Constraints

- 固定原版归档SHA256 `464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`；原版pred.c/pred_template.c分别为`27eab80002d493efadadbe55309d3f2be7359c24d349f582edf3d2ffbb35f9e2`/`70fe448519c1806d416a45afe5e4fb89ec8269809b606ac2821e30b67d82788a`。
- 只新增0003、reference.c、native-tests.c；不改0002、旧16证据、正式recipe/builder/WF、广告、GUI或默认策略。Windows ARM64 COFF/SEH后继保持OPEN，x86系列不受影响，禁止Mac Intel。
- Phone/PC HOLD；不使用凭证、设备、网络或下载代码执行。writer先仅SOURCE；独立审查后root按阶段释放固定编译/运行，不覆盖接受树/对象/原件。
- 原型候选不等于完整FFmpeg软件解码或生产资格；逐目标性能、普通配方/包及真实同流解码独立验收。

## Review Focus

- mode18..25负角使用floor；±32/零fraction不读取第二个向量，末端不得越界。
- mode26在size32不执行luma边界修正，c_idx0/1/2均须保持原结果。
- mode2..17保留同TU原水平路径，不以全新转置器覆盖已向量化代码；不可用可变全局函数指针保存fallback。
- stride正负、三地址mod16和top==left均保持完整padding/input；dst与refs不重叠。
- CPUoff/非8位/未知位深保原完整context；CPUon的旧16槽不因新增32被当作回归或偷偷改写。

---

### Task 1: 原版／模型及真实selector RED

**Files:** Create `third_party/ffmpeg/8.1.2/tests/hevc-angular32-reference.c`; Create `third_party/ffmpeg/8.1.2/tests/hevc-angular32-native-tests.c`。

**Interfaces:** 原版include TU只将ff_hevc_pred_init改名frd_hevc32_original_init；保持原pred.c/header/template不改。callback沿HEVCPredContext的六参数angular签名：`void (uint8_t *dst, const uint8_t *top, const uint8_t *left, ptrdiff_t stride, int c_idx, int mode)`。未来C wrapper名`ff_hevc_pred_angular_32_8_dispatch_aarch64`，汇编核名`ff_hevc_pred_angular_32_v_8_neon`。新reference暴露reference/selector两个固定执行模式；native用例复用同一case/model而不复制候选算法。

- [ ] 先写原版/model tests：N32，mode2..34×c_idx0/1/2；显式floor_div，不依赖负数右移/向零除法。原版TU和独立model逐byte对照完整1024像素、整dst/padding及输入不变。
- [ ] 固定46类图案沿旧16图案定义扩展至32和端点63；strides32/33/63/64/128/257/-33/-63，dst8192B，refs各128B含合法[-1..63]。数值组全图案、布局组3图案各stride独立扫三地址mod16、同址refs组全图案；记录精确case数，不能声称未测的交叉乘积。覆盖fraction取整和size32无边界修正判别。
- [ ] 完整context验证：CPUoff/on和8/9/10/12/0/7/16；0002旧16符号保持，不跨TU用函数地址证明原版身份。RED专属never-called wrapper marker仅供取地址，REQUIRE_NATIVE与RED宏互斥。
- [ ] 源码独立审查后root绑定现固定原版tree、配置、compiler/avutil及0002对象，释放本任务所需有界host原版/model运行。预期reference exit0且完整case终态；真实post0002 selector的32槽仍为C，精确marker未选中才exit3。任何编译/链接失败、signal、marker被调用或其它context异常不算RED。
- [ ] root取得上述原流并验收后，Task2才可写实现。旧16 ABI/guard/benchmark不重跑。

### Task 2: 垂直32汇编及新增路径验收

**Files:** Create `third_party/ffmpeg/8.1.2/patches/0003-aarch64-hevc-angular32-vertical-neon.patch`; Modify仅本任务两个新C测试以接入GREEN。

**Interfaces:** patch仅沿0002 postimage增加新ASM及C wrapper/真实slot3 hook；wrapper在pred.c同TU可访问原static pred_angular_3_8，aarch64 init声明wrapper并仅在原exact8+NEON条件下设置slot3。禁止全局可变fallback。所有其它尺寸/位深/槽不变。

- [ ] 以Task1真实RED为基线实现新ASM：仅mode18..34，signed stride，NEON按32byte row插值/精确copy。负角ref构建遵原floor/inverse索引；数值分子最大8160，round+16最大8176，u16足够。不得无条件做fraction0的第二参考读取。
- [ ] 原水平C body保留；新wrapper只作方向选择。汇编不碰x18、nonvolatile/LR；SP16对齐，scratch/CFI与实际指令相符。沿现Mach-O/ELF私有边界，COFF不假借此门取得资格。
- [ ] 独立源码审查后root释放当前候选的必要实际selector GREEN/数值/最小读取guard/直接ABI/linked ISA及精确rounding/index/direction/越界负控；只覆盖新增32路径，保旧门与旧失败原件。
- [ ] 运行结果和平台矩阵随候选提交更新；性能/各目标容器/正式配方及包、完整同流解码后续分别验收。未完成不得改广告或默认策略。

## 执行安排

沿用户已批准的主计划和AGENTS默认subagents执行：root设计、复核、集成、Git与实际执行释放；单一writer独占本任务新文件，另一agent独立审查。不新增用户确认步骤，不替换已批准的执行方式。新候选逐阶段验收，任务状态不缩减完整goal。
