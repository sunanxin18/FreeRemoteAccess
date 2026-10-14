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

## 剩余验证

两实例 Stage 原版/兼容/NEON 对照、完整流水线性能及分配观察尚未完成。宿主已有的 38 项完整流水线和 35 项 Stage 性能
未达标记录保持，见[宿主整管线验证](rdp-legacy-pipeline-20261014.md)。

cursor、SSE2 publication、bulk、正式解码器选择、普通 HAP 接线和真实 Windows/
Mac 登录继续按完整计划推进。上述两个独立目标的处理链路测试通过，仍不代表客户端已能远程控制 Windows
或 Mac。
