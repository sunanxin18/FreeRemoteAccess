# 鸿蒙 ARM64 整管线正确性：手机与 PC 模拟器

2026-10-14，Task 4A2.10A。状态为**开发中**。手机与 PC 模拟器分别完成本页四项实际正确性
测试。性能、普通 HAP
协议注册和真实远程登录仍未完成。

## 冻结输入与执行层

使用提交 `618168c286bde900c6838098c76021bfc4910d42` 的独立源码快照，Rust
1.96.0 和 DevEco API26 OHOS ARM64 工具链。六项测试源码及全部 266 项生产
源码身份与冻结快照一致。正常、独立原版、分配观察器分别构建为 release ELF，
实际 Cargo JSON、fingerprint 和 readobj 核对通过；三者仅直接依赖 `libc.so`。

正常测试 ELF SHA-256：
`722a257b04c52df980bb78c122de2a1c70a5d274fbeb926aa6de4d8333e85d26`。
正常及独立原版 rustflags 为空，分配观察器使用独立 cfg 和输出目录。

固定 74 组数据 SHA-256 为
`198c9e0a9ce86ff9c0ca8b916d36bb5eef9ad211dd6ffd3b306e3e396d19ae4b`；
持续状态参考 sidecar 为
`15f70cf65f485695807e83e1ca7a0c19227ea71871e9c7fe08122f8b5ba010b7`。
上传后的每份 ELF、数据和 sidecar 均在对应实例按确切路径重新核对 SHA。

执行器独立审查先发现 full 测量输出字段校验不足，修复后 15 项模拟检查实际
通过，独立 Spec/Quality 复审通过。模拟检查仅证明脚本判断逻辑；以下结果来自
手机实例的实际测试程序，不能用模拟检查替代。

## 手机实际结果

实例为 API26 ARM64 手机模拟器 `127.0.0.1:5555`。每项精确选择一个 ignored
测试并显式执行，结果均为 `1 passed; 0 failed; 0 ignored; 0 measured`，宿主
HDC exit 0，设备唯一 `FRD_EXIT=0`。根执行工具 session 71592 最终 exit 0。

| 测试 | 实际结果 | 测试程序报告耗时 |
|---|---|---:|
| `pipeline_original_checkpoints_real_factory_to_canvas` | 完整 74 workloads、两种 backend 对照通过，`subset=false` | 645.17 s |
| `pipeline_regression_discriminators` | 回归判别通过 | 0.08 s |
| `pipeline_steady_pointer_reference_gate` | 持续指针状态参考通过 | 111.45 s |
| `pipeline_stage_steady_pointer_reference_gate` | Stage 持续指针参考通过 | 1.68 s |

这些是正确性测试耗时，不能作为性能基准。首项覆盖实际 factory→Stage→
baseline/Surface→mailbox/compiler/canvas 处理链路，并保留畸形输入及失败语义。
本次没有 HAP 窗口、网络认证或远端桌面输入。

原始日志、每项实际 PID/argv/终态及确切日志 hash 保存在本地
`target/harmony-pipeline-runtime-20261014/actual/phone/`。
`correctness-complete.json` SHA-256：
`4cc72f0ba89597bdb1651db5f4ad6b01d32d2f93d8c72285e8c4cf3e143bc548`。
root 重新读取四项原始结果并重验当前日志身份与执行器的同实例前置门，没有
重跑已通过项目。

## PC 独立实际结果

PC 实例为 API26 ARM64 模拟器 `127.0.0.1:5557`，在手机完成后串行执行。
根执行工具 session 61707 最终 exit 0；四项均为精确 1 PASS、0 failed/ignored/
measured，宿主 exit 0 和设备唯一 `FRD_EXIT=0`。

| 测试 | 实际结果 | 测试程序报告耗时 |
|---|---|---:|
| `pipeline_original_checkpoints_real_factory_to_canvas` | 完整 74 workloads、两种 backend 对照通过，`subset=false` | 706.97 s |
| `pipeline_regression_discriminators` | 回归判别通过 | 0.09 s |
| `pipeline_steady_pointer_reference_gate` | 持续指针状态参考通过 | 112.44 s |
| `pipeline_stage_steady_pointer_reference_gate` | Stage 持续指针参考通过 | 1.91 s |

原始证据位于同级 `actual/pc/`。`correctness-complete.json` SHA-256：
`fd3da87d10a0261321d76f56a426151c7693dbb638c8eb5bd1075ce59424e8a2`。
root 独立重验四项原始日志、终态与同实例前置门。没有将手机结果代替 PC
结果，也没有重跑已通过项目。

## 手机 Stage 初始测量

2026-10-14，同一冻结 618168c 的手机 Stage 初始测量进程 session 67139 已正常
结束，宿主 exit 0。协调计数的六轮共 12 个任务均达到实际终态，原版进程及
当前进程各自输出完整记录，当前进程的精确测试为 1 PASS、0 ignored，设备
终态为唯一 `FRD_EXIT=0`。root 核对了 124 个记录文件的身份与最终 174 组
O/C/N 结果，没有重跑已完成的正确性或初始测量。

初始次序下保留 18 项 N/C p95 比值超过 1.25 的记录；128 个原版组在每组
4096 次计数上限下仍不足 100 ms，必须保留时钟粒度限制。它们不是最终顺序
平衡结果，也不证明完整管线或生产性能资格。最终初始 receipt
`stage-initial-complete.json` SHA-256 为
`064399b56e4a80bfb6657e9f7ac93e075fc6c2d6ac3d4f693cc9d075c48472e0`。

随后仅运行全部 58 个 workload 的 window 1 顺序补充，原版先行，当前进程
采用 N/C 次序。根 session 49432 实际 exit 0；补充两轮共四个进程均正常结束，
第一轮接受 57 组，另 1 组按原固定协调规则增至 2048 次后接受，没有重跑不利
性能结果或修改阈值。root 核对全部 171 个新旧记录身份、四个实际进程终态、
174 组 O/C/N 计数、校验和与顺序，确认初始 window 0、2 的行和原版 JSON
逐项保持不变。

最终平衡结果仍有 18 项 N/C p95 比值超过 1.25；原版 128 组不足 100 ms，
129 组达到 4096 次上限，全部保留。这是测量完成及性能未达标的证据，不是
生产资格通过。`stage-final-complete.json` SHA-256 为
`9c492a8fbafc694d2f00f2b5e80d971cbeaf02b92b9e91545e04c9d73ccf0d8b`，
独立 root 核对记录 SHA-256 为
`85237f1c3c367b3cead0f909c137a39e4c1c0cef454b54de5d4380c1f65af9d3`。
PC Stage 的初始与顺序补充测量已完成，完整管线和分配观察仍未完成。当前 native.rs 优化候选不属于
本次冻结程序，不将其源码变化归因于这些测量。

## PC Stage 初始测量

同一冻结 618168c 的 PC 初始阶段已实际结束。恢复工作时原执行工具句柄不再
存在，宿主 PID 3357 也已退出；root 核对十二个实际任务的终态与原始日志，
每项宿主 exit0、设备唯一 `FRD_EXIT=0`，六个当前程序测试各精确 1 PASS。
没有因观察句柄消失而重新运行任务。

六轮协调分别处理 174、21、7、3、2、1 组，按原固定规则保留全部记录，最终
174 组计数、校验和、冷启动合计与实际工作量均一致。root 重验 124 个产物
指纹，并从各轮原始记录重新组成最终结果。初始结果有 20 项 N/C p95 比值
超过 1.25；原版 128 组不足 100 ms，129 组达到 4096 次上限，全部保留。
这些是初始次序结果，尚未完成 window1 原版先行的顺序补充，不授予性能资格。

`stage-initial-complete.json` SHA-256：
`d3d12faf88b862514e0fe61fd1d5558e4799ed0c72d48d31b56fb19554d06bba`。
阶段终态 SHA-256 为
`ea74d5fb1caebb1a7a1f068e34cdd1619affa9752cc998ce5589de3f8e7cac84`；
独立核对记录 `root-pc-stage-initial-verification.json` SHA-256 为
`6abcc00d3e8278c02b72dd2d88476d5e3cb6b4d35eea5b6327e356099266b92a`。

随后按原计划仅补充全部 58 个 workload 的 window1，原版先行、当前程序
N/C 顺序，session42949 实际 exit0。两个任务分别正常结束，原版约 146.85 s、
当前程序约 286.04 s；当前精确测试 1 PASS、0 ignored，均宿主 exit0 与设备
唯一 `FRD_EXIT=0`。一轮接受全部 58 组，无重试；重启的是此前已关闭的专用
模拟器，没有重复初始测量或正确性任务。

root 核对 149 个新旧产物身份，确认初始 window0/2 的行和原版 JSON 不变，
补充原始记录、实际计数、校验和、冷启动顺序与工作量组成最终 174 组。
平衡结果保留 21 项 N/C p95 比值超过 1.25，原版 129 组不足 100 ms、129 组
达到 4096 次上限，不授予性能资格。最终 receipt SHA-256 为
`15fd290c345540d68c02f0bb260a7ce59a16b44d10cdd7d0d371003af13fac8b`，
阶段终态为
`c72a55b4ffd7108e4a12c9dac9b91acffeccbda5cf4047511fea1bb07eaf4ce9`；
独立核对记录 `root-pc-stage-final-verification.json` SHA-256 为
`23635b05d5a3a3052f73dea19a5f894e868de803bc6237914b29bb97a870b7c2`。

## 剩余验证

两实例 Stage 原版/兼容/NEON 测量过程完成，性能未达标及短测量限制保持。
完整流水线性能及分配观察尚未完成。宿主已有的 38 项完整流水线和 35 项 Stage 性能
未达标记录保持，见[宿主整管线验证](rdp-legacy-pipeline-20261014.md)。

cursor、SSE2 publication、bulk、正式解码器选择、普通 HAP 接线和真实 Windows/
Mac 登录继续按完整计划推进。上述两个独立目标的处理链路测试通过，仍不代表客户端已能远程控制 Windows
或 Mac。
