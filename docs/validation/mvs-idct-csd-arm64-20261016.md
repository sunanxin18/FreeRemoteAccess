# Apple MVS ARM64 CSD IDCT（2026-10-16）

状态：**实验性，默认关闭**。本轮只将私有 i64 双 lane 常量乘法改为编译期 CSD/NAF 移加减序列。正确性、实际 OHOS ELF 和独立源码审查通过；手机模拟器固定对照完成，性能结果有改善也有退化，**性能晋升门未通过，仍 OPEN**。不改变默认后端、codec 广告或生产资格。

## 范围与正确性

唯一产品变更为 `crates/frd-protocol-apple/src/mvs_idct_arm64.rs` 的 `multiply<C>`、对应界证明及新测试。保留双 lane i64、原蝶形、两遍11/18舍入、范围映射、标量 oracle、wire、事务和现有 feature/架构门。没有增加 DC 快捷路径；macOS 仍仅支持 ARM64，其他架构没有由本轮获得新资格。

先加入同一直接测试并冻结当前源码的原 helper 为 N0，再修改 helper 构建 N1；旧 fb78 ELF 只作历史正确性参考，未作为本次性能基线。79个当前项目输入仅此文件变化，N0/N1 从 transform 开始的实现与测试后缀逐字相等。实际编译读取的65个本地输入包含于冻结闭包；694个 registry 编译输入前后相等。完整 registry 和非 Apple 缓存工件身份是在最终阶段观察，并结合 Cargo fresh/mtime 检查，不能表述为 N0 时已复制完整依赖缓存。

12个实际常量的 CSD 绝对位权和最大41,556，helper 输入界为4W=2,908,534,084；每一步移位、抵消前部分和及负常量取负均不超过120,867,042,394,704，小于原蝶形界和2^47。两个 lane 不窄化、不依赖溢出。直接测试用独立 i128 乘法比较完整 i64 结果，覆盖24种带符号常量、10,200个向量/20,400个 lane；独立非相邻表示模型检查恒等式与界。

宿主 N0/N1 直接测试各1通过；N1 聚焦12通过，完整 Apple aggregate **555通过、11 ignored、0失败**，含已有28,800块差分。宿主三组完整 wire 输出/状态比较各386,897,350字节相同，像素、矩形、顺序和状态四种负对照均拒绝。作者早期类型推断、输入闭包和反汇编解析器错误保留，不伪称功能失败或实际 scalarization。

## 最终工件与静态检查

相同 Rust1.96、Release opt-level3、locked/offline、`mvs-native-arm64` 和官方 API26 SDK 构建；两版实际 Apple 工件均重新编译。OHOS ARM64 测试程序直接依赖 libc++_shared.so/libc.so，SDK C++ 库身份固定。

| OHOS Release libtest | bytes | SHA-256 |
|---|---:|---|
| N0 原 helper | 7,321,312 | `e699c7bc019801f00b25b6ef4398aec04ff0ff57c455e8f909541e3815165ba8` |
| N1 CSD helper | 7,319,272 | `18139f8153d889f8dc53c486aae645fdafeeea0045e28fda61fcaec14ac6b636` |

独立审查核对实际 ELF symbol、全部入口机器字、指令与 full/type-1 调用。IDCT 静态指令665→542，ADD/SUB/SHL .2d 合计369→281，栈640→576字节；保留原两个循环分支，无运行时 digit 循环、GPR乘法、i64 lane提取或 helper 调用。静态减少不能换算为 cycles 或速度提升。原源码审查 Spec PASS / Quality APPROVE，执行后报告处置亦获独立 APPROVE；两者都没有授予生产性能资格。

## 手机模拟器资格与固定计时

仅通过官方 HDC 在 API26 ARM64 手机模拟器运行原生 CLI；不访问产品 UI、HUKS、网络或 HAP 生命周期。PC 仍处于人工填写阶段，本轮未观察或操作 PC。

N0/N1 各一次直接 helper 测试和一次32 record/8 state资格，均通过。13条 host 命令退出0，其中8条 guest 命令各有唯一退出0记录；不是13次 guest 测试。取回两份各41文件，N0/宿主N0、N1/宿主N1、目标N0/N1三组逐字比较各386,897,399字节相等，包含49字节 DONE。

随后另行释放固定 N0-forward、N1-forward、N1-reverse、N0-reverse 四进程，每次只执行一次，host/guest均0。合计 **384 measured、128 warmup**；每 backend/case 有24个 measured 点，每固定 record 有6点。每条计时后完整输出、每 cycle 完整状态比较均通过；所有原始点、apply/publish/total及min/median/max保留。root和独立审查者分别从原始记录重算统计一致，无增加样本或跑到变好。

| case | N0 total 中位 µs | N1 total 中位 µs | N1/N0 |
|---|---:|---:|---:|
| full-copy-720 | 2434.7500 | 2481.0625 | 1.019021 |
| full-copy-tail | 2801.4170 | 2930.9170 | 1.046227 |
| clipped-overlay | 7.0830 | 6.7920 | 0.958916 |
| local-copy | 23.0210 | 22.1460 | 0.961991 |
| local-miss | 149.4585 | 164.5830 | 1.101195 |
| local-type1-copy | 24.5835 | 25.3330 | 1.030488 |
| full-dense-y | 12614.2290 | 11861.3335 | 0.940314 |
| local-type1-refine | 136.6460 | 138.3960 | 1.012807 |

dense-Y 总量中位约低5.97%，四个 record 比值为0.922889/0.934130/0.946518/0.957499；copy、miss及refine合并中位更慢，必须同时保留。refine 四个 record 的中位比值为0.984134/0.873622/0.922079/0.919253，均低于1，但合并24点的中位比值为1.012807。它包含快慢两簇：N0排序第12/13值73,375/199,917ns，N1为81,750/195,042ns；合并中位不是四个中位数的平均，不能用分组改善替换预先指定的合并指标。

这是共享 Mac 宿主的有限 CPU apply/publish 对照，记录的1分钟 load 为4.632324..4.995605，存在 DevEco、Emulator及其他后台活动。本项目无并行编译/UI测试；不代表独占环境或统计显著性。共同 libtest tracking、断言、发布路径成本仍在计时范围，local-miss保留诊断成本。未单独测量IDCT耗时，不将退化全部归因背景负载；没有硬件手机、GPU、HAP、网络、实际FPS、远程延迟或真实Mac性能结论。

## 证据与处置

源码与构建冻结：`target/mvs-idct-csd-20261016/final-manifest.json`（528项），SHA-256 `e1684ca0a0598e29c15d23ec90148cea3cfd18bae8d18ccb1ff24235e72c96d8`。N1源SHA-256 `fc7d07d564d1f731829ab3f6421f6b167c7f2908dc2dc05f9e5bb14c615cd6ce`。root独立验收、两阶段执行释放、全部原始命令/退出/记录与统计见 `target/mvs-idct-csd-root-20261016/`；执行证据清单213项、774,712,836字节。审查见 `.superpowers/sdd/2026-10-13-harmony-clients/task-5a7-csd-idct-review.md`，原执行释放绑定的10424字节前缀保留。

本地完整证据保存并验收后，仅清理本轮手机独立临时目录中的93个合成测试文件及7个目录，逐项清单/摘要核对成功，恢复可用磁盘939,380KiB；旧参考目录和PC未动。清理不改变本地完整证据或测试结论。

保留已审查源码作为默认关闭实验；有限测量执行已完成，**性能晋升未通过，仍OPEN**。不追加计时重试、不启用生产选择。旧[有限管线记录](mvs-pipeline-measurement-20261015.md)的 adverse 保留；PC计时、普通HAP Apple、真实Mac、硬件设备与其他架构仍须独立完成。
