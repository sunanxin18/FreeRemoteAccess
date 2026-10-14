# Legacy RDP 完整流水线宿主验证

2026-10-14，固定Rust1.96、macOS ARM64，测试专用程序。完成独立原版registry workspace/process到实际工厂选出的Stage、真实server/input处理、baseline、Surface发布、mailbox、FrameTransactionCompiler与持久独立canvas的对照。没有改生产选择、协议广告、GUI或服务器。

原版74工作量的完整正确性检查实际通过。独立持续状态侧数据解决了鼠标工作量的冷/暖预期差异，保留全部冷wire/预期；三种真实编译语义变体由同一流水线入口准确拒绝。最终正常release未编入分配观察器，另一独立构建保存100个policy样本/300个区间的分配六字段。观察峰值是包含预载夹具与审计buffer的全进程Rust堆，不是生产专属内存、RSS或可加总峰值。

完整C/N测量、控制与观察器均正常结束。Stage最终原版/兼容/原生三方为58工作量×3窗口，warm64、至少256整批，按256共同增加至三方100ms或4096上限；窗口1追加原版先执行、原生先于兼容，其他两窗保留原顺序。三方计数及checksum一致，原始初轮数据仍单独保留。独立复审Spec/Quality PASS，根代理复核六源码、三实际程序、23证据、四数据身份及266生产源码。

## 性能结果及范围

全链保留38项预设预算未达标：31项原生/兼容p95比值超过1.25，7项绝对warm时间超标。1920×1080 RemoteFX原生路径RLGR1三窗p95为41.774391/39.324368/38.893475ms，RLGR3为40.311115/35.230371/65.139026ms；33.34ms目标未全部满足。冷setup+真正首次Startup均低于250ms，最大37.899291ms；首次Startup/替换恢复控制通过。六个overflow样本只有诊断时间，没有虚构预设阈值。

最终Stage比值未达标35项，初轮34项另存。达到4096上限仍低于100ms的小样本明确保留低计时粒度限制；不缩短慢路径或改变输入/阈值来取得通过。Stage-only不与全链混算提速。

## 证据与后续

源码复现入口为`tools/tests/run_legacy_pipeline.py`；完整raw日志、CSV、identity与不可变旧证据在`target/pipeline-evidence-rust196/`。`final-evidence.json` SHA256为`9f5bd6468074bcfb91ab5c3a7bd730fed50794d66e1f0c2d261285774a6ac81e`；数据`workloads.bin`为`198c9e0a9ce86ff9c0ca8b916d36bb5eef9ad211dd6ffd3b306e3e396d19ae4b`，暖状态sidecar为`15f70cf65f485695807e83e1ca7a0c19227ea71871e9c7fe08122f8b5ba010b7`。大数据只保留本地，仓库保存来源和紧凑元数据。

已修复统一invoke只认真实exact-one通过，以及继承Rust flags可能混入观察器的两处测试门问题；实际0-test成功退出被拒绝，冲突flags下两构建的fingerprint/list与原程序bytes一致。旧完整正确性证据只在源码/调用前缀/数据/生产闭包不变且实际旧verifier通过的条件下复用，未重复不变大测试。

本结果仅为宿主离线层。手机/PC目标执行、目标性能和优化、软件cursor SIMD及规范数学、SSE2 publication、bulk/ZGFX、普通HAP加载、网络/呈现、TLS/凭据及真实Windows/Mac互操作仍未完成。八vendor像素格式和source-crop辅助对照不虚报为同wireadapter能力，no-bulk与未启用QOI/QOIZ范围明确。不能据此注册生产codec或宣布鸿蒙已经可以远程登录。
