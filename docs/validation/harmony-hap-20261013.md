# 鸿蒙原生 HAP 与图形路径验证

日期：2026-10-13。实现提交 `822c359067ae9bdf363c3af2945d50e6413fc419`；宿主 macOS ARM64，DevEco API 26。两处错误处理问题已在 `2636a6c` 修复并通过独立差异复审；官方 Emulator CLI 已验证图形、横竖屏和生命周期。完整产品 UI、凭据与远程会话仍未完成。

## 已实现与证明范围

原生 ArkUI Stage 工程声明 phone 与 2in1，C++ N-API/XComponent 桥接版本 1 C ABI。Rust engine 使用不复用的整数句柄，surface 使用独立递增代际。每个 engine 的渲染线程串行操作 NativeWindow/GPU；挂接在回调借用有效期间取得独立引用，拆除先停止呈现、销毁 GPU surface，再释放窗口引用。中立 compositor 和 renderer 沿用现有实现。

当前应用只提供明确标识的**离线图形验证**，没有远程登录、协议解析、远程输入或 HUKS 接线。ArkTS 不接收像素，也不产生连接成功事件。普通启动不会自动进入 POC；显式 debug 意图可以进入，release 编译配置和 Rust ABI 都拒绝调试绘制入口。

| 验证层 | 本轮结果 | 证据边界 |
|---|---|---|
| 宿主测试 | renderer 30、compositor 13、desktop 168、OHOS FFI 11，共 222 通过；FFI release 12 通过 | 包含真实宿主 Metal limits 策略测试；不是 OHOS/Windows 实机测试 |
| 根包测试 | `cargo test` 166 通过、2 忽略 | 不是整个 workspace 每个包的全量测试 |
| 静态检查 | 受影响 crate fmt、FFI clippy `-D warnings`、shell/Python 语法及 diff check 通过 | 不替代 GUI 和远程互操作 |
| OHOS 构建 | debug/release 的 Rust cdylib、C++、ArkTS、HAP 均成功 | 仅 ARM64，未新增 macOS Intel 支持 |
| 包结构 | phone/2in1，仅 INTERNET 权限；桥接库、Rust 库及官方 OHOS libc++ 都是 ARM64 | 检查 ELF、SONAME、DT_NEEDED、SDK libc++ build-id 与 OHOS 标识；拒绝宿主路径依赖 |
| 调试签名及安装 | 官方工具 verify-app 通过；两类包均在两个专用模拟器正常安装 | 使用 normal APL、精确设备范围的开发 profile；不是生产分发签名 |
| 手机原生路径 | attach 成功，1088×1532 图形 surface，提交/呈现计数递增 | 是局部 POC surface 尺寸，不是远程 Windows 分辨率 |
| PC 原生路径 | attach 成功，2000×822 图形 surface，提交/呈现计数递增 | 实际最大化/还原/拖动边框已验证至 3030×1383、1408×591 和 888×395 |
| 普通启动和 release 门 | 两台普通 debug 启动不进入 POC；release 显式 POC 意图被拒绝，未挂接 surface | 与模拟器程序实际日志交叉验证，未靠编译宏推断 |

呈现计数只在 compositor 确认提交且 render/present 错误作用域成功后增加，跳过 acquisition 不记为帧。计数证明该原生调用路径执行成功，不能单独当作视觉验收。本轮另行检查了实际截图，两个模拟器的四色及白色十字均完整可见。

## 实际运行发现并修复的问题

1. **库没有 SONAME。** 首轮已签名包中的 C++ 桥接库把宿主 Rust 库绝对路径写入 DT_NEEDED，模拟器无法加载。现在仅 OHOS cdylib 设置固定 SONAME，包检查器拒绝所有依赖中的路径。
2. **模拟器调试 EGL 上下文崩溃。** 普通故障日志定位到系统 `libdgles` 的 `d_eglCreateDebugMessageBuffer` / `eglCreateContext`。仅 OHOS 组合入口移除 wgpu DEBUG 标志后，两台不再出现该崩溃；VALIDATION、GPU 错误作用域及 device-lost 处理保留。尚未将此模拟器结果外推到真机。
3. **默认 GPU 限制超出 GLES 3.0。** 两台均报告 compute workgroups 请求 65535、可用 0；WebGL2 默认 varying 请求 15、可用 7。OHOS 显式选择不要求 compute 的限制策略，并使用 adapter 的分辨率、对齐和 varying 限制。新增通用 `request_with_limits` 参数接口，仍使用真实 surface 选择兼容 adapter；原有默认入口行为保持不变，核心代码不加入 OHOS 分支。

## 签名边界

官方 SDK 示例 Application Release 证书实际为自签名，直接拼接 Application CA/root 的尝试被 sign-app 正确拒绝。随后通过官方 generate-app-cert 获得匹配的 30 天开发证书链，再签署 normal debug profile 与 HAP，verify-app 和两个模拟器安装均通过。

只在 gitignored 的本地目录使用 SDK demo keystore 副本，未修改 SDK、guest trust、权限或签名校验；密码通过交互输入，不放入 argv、普通配置、源码或日志。设备标识、证书、私钥和签名包均不纳入 Git。release 构建也使用该调试 profile 做本地门禁验证，不能因此称为可发布的生产包。

## 剩余验收

- 完整产品的安全区、键盘焦点、明暗主题、文字缩放和完整图标检查；Task 2 独立代码复审、POC 颜色/按钮/方向/窗口尺寸/前后台检查已通过。
- 用户已明确选择官方 Emulator CLI 作为界面验证方式；此前 CUA 无法选中独立裸 Mach-O Emulator，IDE 投屏列表也无可选设备。授权后只操作两个专用测试实例。
- 原生 HUKS、资料与证书持久化、完整 AppController/SessionHost 接线、触控/键鼠、Windows RDP 与 Apple 协议真实会话仍属于后续任务。
- Bitmap/RemoteFX 软件解码的精确 SIMD、OHOS FFmpeg ABI/打包以及 fdk-aac native 依赖需独立处理。现有 RDP `LegacyOnly` 和全部生产能力门未改变。
- 鸿蒙手机被控仍受系统原生服务入口与协议证据限制，见[原生服务调查](harmony-native-service-20261013.md)。本地 HDC 只用于测试，不是产品远控链路。

本地原始日志位于 gitignored 的 `target/`，包括 `harmony-task2-regression.log`、`harmony-task2-ffi-release-tests.log`、`harmony-task2-package.json`、`harmony-task2-sign-verify.log`、`harmony-{phone,pc}-final-native.log`、普通启动和 release 门禁日志。完整实施与审查记录在本轮 SDD workspace。镜像版本及实例范围见[环境证据](harmony-environment-20261013.md)。

## 官方 Emulator CLI 实际界面验证（首轮）

2026-10-13 14:06–14:10，用户明确授权后执行截图、控件树、点击、旋转、滑动与电源模拟。phone/PC 原生截图分别为 1256×2760、3120×2080；可见的是开发 POC，并无远程桌面或认证成功声明。

- 两台初始截图均可见完整四色与居中白色十字，文本与停止按钮清晰；PC 保留系统标题栏、最小化、最大化和关闭按钮，应用图标在标题栏和桌面可辨认。
- PC 最大化使 surface 从 2000×822 更新为 3030×1383，还原后回到 2000×822；拖动窗口右下角后为 1408×591、888×395，计数继续增加。窄窗口说明文字正常换行，系统 chrome 未被自绘替换。
- 手机返回桌面和 PC 最小化后，日志 `foreground=false` 且计数在连续多秒内不增长。PC 从任务栏恢复后，同一 engine 继续绘制；手机电源关屏再上滑解锁后，同一 engine 恢复。启动器图标会发送新的普通 Want 并退出调试模式，因此另外使用上述恢复路径验证，未将重新启动当成原会话恢复。
- 两台点击停止后 XComponent 从控件树消失、日志 `detach code=0`；再次启动产生新 engine（2→3）并重新呈现。无真实远程输入，按钮点击只证明本地图形操作。
- 手机首次旋转时系统旋转锁定启用；关闭后截图确认锁定未启用，但应用布局仍保持竖屏尺寸。这是未通过项，已交给实现代理核对应用方向策略，未将物理模拟器外壳旋转计作应用横屏适配。
- 明暗主题、字体缩放、键盘焦点和完整登录/会话界面仍留待后续真实产品 UI；本轮没有假设这些已通过。

截图保留于忽略目录 `target/harmony-validation/gui-phone/`、`gui-pc/`；原生生命周期日志为 `phone-gui-lifecycle.log`、`pc-gui-lifecycle.log`。独立代码审查要求补齐 panic payload 脱敏及 NAPI 帧回调注册错误保持，两项进入 Task 2 第一轮修复。

方向修复依据：[华为窗口旋转策略](https://developer.huawei.com/consumer/cn/doc/doccenter-capabilities/window-rotation)和[横竖屏开发实践](https://developer.huawei.com/consumer/cn/doc/best-practices/bpta-landscape-and-portrait-development)明确说明，`auto_rotation_restricted` 跟随传感器并受控制中心旋转锁定开关控制，适合可旋转应用；API26 本地 `@ohos.window.d.ts` 的枚举注释与此一致。最终是否生效仍以新包实际旋转后的 surface 尺寸和截图为准。

## 第一轮修复包的实际复验

新 debug HAP `entry-round1-debug-signed.hap` SHA-256 为 `f682ed450b27c0795fef40009f86e9e818a45272ad6c7f92d696002f0dcbfb6a`，两个实例均正常覆盖安装和启动。14:21 手机横屏控件树已报告 2676×581，截图 `gui-phone/Screenshot_2026-10-13T142129.png` 显示四色十字及可点击停止按钮；切回竖屏恢复 1088×1532。横屏按钮仍为 48vp（168 物理像素），停止后 surface 从控件树移除，重新启动成功。PC 新包窗口和最大化状态分别为 2000×822、3030×1383，持续正常绘制。

横屏截图中，设备摄像头开孔仍覆盖 POC 右边一小块像素；Task 2 只验图形挂接与旋转生命周期，不能据此宣称完整安全区验收通过。Task 3 的产品内容矩形必须纳入 cutout inset，并让渲染和输入共用该矩形。主题、字体缩放、键盘焦点、远程输入也仍未通过。

修复包后台复验：phone PID19814 在后台计数1789不变、恢复后1880；PC PID16158 在后台6943不变、从任务栏恢复后7883。开启系统旋转锁时手机布局保持竖屏，验证后恢复为竖屏、锁定关闭。已通过页面按钮停止两台绘制，模拟器保持运行。截图/日志内容校验值登记于本地 `target/harmony-validation/graphics-gui-manifest-20261013.json`。修复代码提交为 `2636a6c`，独立差异复审已批准：两条 P2 均已解决，无新增严重或重要问题。非阻断的 SDK optional outproof 警告保留；尝试对应参数被 SDK 拒绝，普通签名验证实际通过。
