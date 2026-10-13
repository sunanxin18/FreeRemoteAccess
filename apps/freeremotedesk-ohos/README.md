# HarmonyOS 原生 HAP

API 26 ArkUI Stage 工程，同一清单声明 `phone` 与 `2in1`，当前打包 ABI 仅 ARM64。
基于官方 DevEco Native C++/New Project 模板实例化（modelVersion 26.0.0）。
应用图标使用仓库已审 `assets/app-icon/apple/{foreground,background}.png` 无预制遮罩层，
由 HarmonyOS layered-image 与系统规则组合。功能按钮使用原生文字，没有添加第二图标家族。

当前页面明确为**离线图形验证**。普通启动不自动创建测试表面；debug 构建可通过按钮或
`aa start ... --pb frd_graphics_poc true` 启动四色 POC。release 原生 NAPI 编译门与 Rust
C ABI 运行门均拒绝 POC。此页面没有远程连接、协议解析、密码字段或伪造成功事件。
GUI 基线参考已批准设计中的 HIG Windows、Material 3 按钮和鸿蒙多设备适配资料；
控件为 ArkUI 原生 Button，目标为48逻辑像素，使用中文可访问名称及系统字体/主题。

从仓库根运行 `tools/build-harmony-hap.sh`；`--release` 构建 release。
先选择安装了 `aarch64-unknown-linux-ohos` 的 Rust 工具链（本地要求 Rust 1.96），
脚本配置官方 NDK，再构建 Rust cdylib、CMake/NAPI 和 HAP。默认 SDK 根来自 DevEco Studio；
可用 `DEVECO_HOME` 与 `DEVECO_SDK_HOME` 指定合法安装。依赖路径、ELF ABI、SONAME 与
官方 OHOS libc++ build-id 会在打包后检查。构建不等于签名、加载、视觉验收或远程互操作。

正常开发签名使用 `tools/sign-harmony-debug-hap.sh`，输入合法证书链、已签 profile 和
keystore 的路径，口令由官方工具交互读取。签名材料、HAP 与验证输出留在本地 `target/`
或忽略的 signing 目录；不得提交私钥、profile 设备身份或用户秘密。不要修改系统信任、
权限、签名检查或用未签 profile 绕过验证。SDK bundled demo 材料仅可用于明确授权的本地
开发设备，不能当作发布身份。正常 DevEco/AGC 开发签名同样适用。

C ABI 合约在 `include/frd_ohos.h`。引擎句柄不复用，surface generation 单调递增；
UI 的 NAPI/XComponent callback 不进入渲染线程。每个引擎只有一个 worker，全部应用
NativeWindow/GPU/ref/unref 操作串行执行。attach 同步等待引用取得/初始化或完整失败清理，
不使用超时遗留借用地址。destroy 先封住 tick、同步 detach Surface，再 join worker。
窗口 lease 的 Send/Sync 约束和保留引用的推论见 Rust `ohos.rs`。

当前官方模拟器 libdgles 的 EGL debug context 会崩溃，因此 OHOS composition root
仅移除 wgpu DEBUG 标志，保留 VALIDATION 和 fault/device-lost 观察。两个 guest 的 GLES3
compute workgroups limit 为0、inter-stage variables 为7；显式使用适配纹理/片元 renderer
的 downlevel limits，分辨率/对齐取真实 compatible adapter。没有提升 codec 能力门。
真机、网络会话、音频、剪贴板、安全存储和输入互操作均不是此图形 POC 的证明范围。
