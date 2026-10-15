# RDP coverage 包装器的分配失败注入（2026-10-15）

状态：**受限验证**，仅 macOS ARM64 的实际源码离线测试。四个测试文件已修复并通过矩阵，独立Spec PASS、Quality APPROVE；不代表新的产品解码、性能、包或设备验收。

原默认包装器生成自己的全局 Probe，同时包含真实 image 的 cursor 全局 allocator；optional 模式又包含已有 ProbeAllocator，导致 rustc 重复全局分配器错误。修改前两组均重新复现 exit1，测试尚未执行。默认改为复用 cursor 的线程局部精确一次分配失败状态；optional 仅在本 coverage test 构建设置专用 cfg，注册原有 Probe 并组合 cursor 拒绝谓词。没有关闭 cursor 模块/实际用例、替换生产 image 或取消真实空指针分配注入。

receipt/rectangle reserve 回归在清除注入前检查它已被真实分配消费，仍核对错误链、像素、cursor 和 pending 原子性。cursor reserve 回归仍检查 TryReserveError 与背景/形状/位置不变。optional 的 ALLOCATION_CEILING、FAIL_ALLOCATION、FIRST_ALLOCATION_LIMIT 和超限退出保护均保留。专用 cfg 由本包装器显式传入并登记 check-cfg；其他包装器未获得该配置。

固定 Rust1.96、offline/locked，default/optional 各两项 receipt/cursor 聚焦均1 passed。随后没有过滤条件的完整执行如下：

| 模式 | passed | failed | ignored | filtered | 脚本exit |
|---|---:|---:|---:|---:|---:|
| default debug | 48 | 0 | 0 | 0 | 0 |
| default release | 49 | 0 | 0 | 0 | 0 |
| optional debug | 63 | 0 | 0 | 0 | 0 |
| optional release | 64 | 0 | 0 | 0 | 0 |

release 多一项既有 partial-alpha wrapping 回归。四组最终完整日志无编译 warning/error；首次只登记自定义 cfg 导致 test/feature 警告的日志也保留，随后明确登记了 cfg(test) 与 qoi/qoiz。optional 的四个大分配保护用例另外在 debug/release 各独立子进程执行，八次全部exit0。没有跳过失败或将仅构建当执行通过。

root 已读取完整四文件diff和报告，逐项核对四组终态及 receipt/cursor、可选大分配保护用例的实际结果；全部 crates/third_party 的452个生产源前后hash相同，root重新计算当前文件也相同。生产清单SHA256 `70c391e615a9735334ce0129469ff69ada782aea56c6c4b8f58f277dc8c1e596`，测试diff SHA256 `7febb6509321fb24f925a43ea6902faac3662e6af14d207d131b5f74e15f2ef8`。

完整证据在 `target/rdp-coverage-allocator-20261015/`，包含两次RED、四组完整GREEN、四项聚焦、八个保护子进程、来源hash与root核对记录。未修改生产src、依赖、锁或原版oracle，未为此重建旧版oracle。其他 image/optional/cursor 包装器及其同类分配器冲突尚未执行或修复，不能将本矩阵描述为整个测试生态已通过；Windows/Linux/OHOS目标执行、整管线性能及真实RDP刷新仍分别开放。
