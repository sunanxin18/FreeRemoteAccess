# 鸿蒙手机与 PC 客户端设计

2026-10-13 用户确认沿用 ArkUI + XComponent + Rust 核心方案；被控鸿蒙手机也只能使用系统原生服务。用户授权自动下载缺失的官方模拟器镜像。

## 产品范围

- 同一个 HarmonyOS 原生工程适配 phone 和 2in1，采用各自窗口、输入法、安全区域、触控/键鼠习惯。
- Windows 目标复用 IronRDP adapter；Mac 目标复用现有 Apple HPSS/MVS/High Performance adapter。不在任何被控设备安装配套程序，不使用 Android 兼容层。
- 鸿蒙手机目标单独研究原生服务的发现、认证、画面和输入协议。SDK 的录屏/输入控制 API 不是网络服务。没有证据前不注册一个假可用协议，也不用 HDC/调试输入充当产品控制链路。
- 验收分为本机测试、OHOS 交叉构建、HAP、模拟器呈现、真实协议互操作和真机。各层不能互相替代。

## 已批准架构的落实

选择 ArkUI/XComponent + 原有 Rust 核心。相比 ArkTS 重写协议，保持协议/解码一致性；相比复用桌面 winit 壳，避免把桌面生命周期和交互硬搬到手机。

1. 从 desktop crate 提取已有中立 SessionHost、cleanup、video worker 和所需值类型到 `frd-shell-runtime`，桌面使用 re-export 保持原接口及行为。不得复制状态机。decoder registry 由 composition root 注入，不能在中立 worker 内自行加载 Linux/Windows FFmpeg。
2. `frd-shell-ohos-ffi` 提供版本化 C ABI：opaque engine handle，固定宽度 POD 和调用期借用 UTF-8。返回缓冲区有唯一配对释放入口；panic 不跨 FFI；销毁、断开、后台、重复 detach 幂等。密码不出现在状态快照、异常或诊断中。
3. `frd-platform-ohos` 实现 profile metadata、HUKS 保护凭据和证书 pin，使用应用沙盒，不复用 Linux Secret Service。密钥不可导出；密文可在沙盒持久化，明文只在安全输入与连接所需内存中短暂存在。
4. `apps/freeremotedesk-ohos` 为 ArkUI 工程及 native 桥。ArkTS 只交换低频意图和状态，像素留在 native Rust 渲染路径。XComponent 绑定 OHNativeWindow，由 owned lease 保持引用。
5. 沿用 pinned wgpu GLES + `frd-render-wgpu`/`frd-compositor-wgpu`。生命周期固定为停止呈现、销毁 Surface、清除窗口访问、释放 native window 引用。渲染与输入共用同一物理内容矩形；旋转、缩放、键盘避让更新同一 geometry generation。
6. OHOS Rust target 的 `target_os=linux,target_env=ohos` 必须显式与桌面 Linux 区分，不能误加载 Linux FFmpeg/GTK/Secret Service。协议 wire 层不增加平台窗口分支。

## 交互与安全

手机为触控优先，登录表单自适应，远程视图支持平移/缩放与明确的输入模式；PC 保留系统标题栏及居中的会话操作。首次证书记录并连接，相同指纹继续，变化停止自动连接。输入只在前台、焦点归远程内容且当前 generation 完整基线已呈现时发送；失焦/后台/断开 ReleaseAll。

RDP 能力门保持现状：生产 LegacyOnly；实际 AVC/HEVC wire/profile/decoder/架构/包/互操作门未全通过不广告。软件热路径使用已有汇编/NEON；鸿蒙 native codec 接在既有 media 契约下。不可用音频、剪贴板及 codec 必须显示其真实状态。

HIG、Material 3 的可访问性、焦点和交互基线继续适用；具体窗口与控件遵循鸿蒙 ArkUI 平台能力，手机不复制桌面标题栏，PC 不复制手机导航。功能图标仍使用现有 Material Symbols Rounded 资源和许可证。验证明暗主题、字体缩放、触控目标、横竖屏、IME 与 PC 窗口缩放。

PC 标题栏采用[华为官方自由多窗沉浸式方案](https://developer.huawei.com/consumer/cn/doc/doccenter-dev-faq/faqs-arkui-1089)：`setWindowDecorVisible(false)` 隐藏默认标题区但保留系统三键，`setWindowDecorHeight` 设置高度，`getTitleButtonRect` 与 `windowTitleButtonRectChange` 驱动避让。产品会话控件在原标题区按窗口几何中心排列；远程内容从该区下缘开始，渲染与输入使用同一矩形。不得在系统标题栏下另加工具栏，也不修改系统 ArkUI 的 titlebar 构建文件。正常权限 API 可用性、系统三键、拖动、双击、窗口缩放、全屏和窄宽折叠仍需实际 GUI 验证；接口文档不是运行通过的证据。

2026-10-13 设计资料核对：[Apple HIG Windows](https://developer.apple.com/design/human-interface-guidelines/windows) 要求适应窗口大小并保留系统窗口行为；[Material 3 Text fields](https://m3.material.io/components/text-fields/guidelines) 与其[官方组件文档](https://github.com/material-components/material-web/blob/main/docs/components/text-field.md) 为标签、支持文字和错误语义的基线；[华为多设备适配指南](https://developer.huawei.com/consumer/cn/multidevice/adaptive-apps/) 要求布局和交互随窗口空间动态响应。M3 主站在当前网络下正文不可读取，具体 GUI 实施前仍需核对可访问的官方组件资料。没有批准省略持久标签、焦点、触控尺寸或系统窗口行为的例外。

## 当前工具链证据

本机 DevEco Studio 26.0.0，SDK API 26 / 26.0.0.105，Hvigor 6.26.4，ohpm 26.0.0.630，Node 24.14.1。PATH 的 deveco 0.2.1 是 AI agent CLI；构建实际调用 Studio 的 Hvigor、OHPM、CMake/Clang 和 HDC。

已有的 Pura 90 Pro ARM64 实例保留不变。已通过官方 CLI 下载 2in1 ARM64 7.0.0.107 镜像，并创建、启动独立的手机/PC API 26 实例；两者 HDC Connected、boot complete、arm64-v8a。手机镜像包 7.0.0.106，PC 包 7.0.0.107，两个 guest 均自报 OpenHarmony-7.0.0.105。Rust 1.96 OHOS std 已安装，现有 core/frame/app 的 OHOS target check 通过；尚无 HAP 或协议互操作证据。详见[环境验证](../validation/harmony-environment-20261013.md)。

## 保留的未决门

被控鸿蒙手机的系统原生远程服务尚未确认；不会以自建 server、HDC、修改系统权限或注入服务替代。Mac/Windows 授权目标的地址和密码从本机安全提供者读取，不进入工程。HAP 签名若需要用户账号认证，保留已构建产物并通过正常开发签名流程完成，不伪造系统权限。
