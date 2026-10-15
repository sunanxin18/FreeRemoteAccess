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

## 鸿蒙系统编解码优先（2026-10-13 用户决定）

鸿蒙使用 AVCodec Kit 原生能力，在既有 `VideoDecoderRegistry`/解码器契约下独立接入。选择顺序为满足精确码流与生命周期要求的系统硬解、满足同样要求及软件 SIMD 规则的系统软解、最后为随包 FFmpeg。逐设备查询并试解码；库存在、Create 成功或 MIME 匹配均不等于精确 profile、位深、色度、输出格式、分辨率与低延迟可用。系统软解实现的汇编/SIMD 证据仍需核验，不能仅凭名称推定。

优先验证系统 ByteBuffer 输出及其裁剪、stride、slice height、缓存和回收生命周期，再接入统一帧契约。Surface 路径若采用，必须与现有呈现、完整基线和 generation 门保持一致。硬件资源不足或精确格式不支持时，只能选择已经验证的后备；切换需要清空旧解码状态并重新建立完整基线，不能复用旧帧。

当前 API26 两台模拟器的三组实际测试均不能在单帧后、无需新输入或 EOS 的条件下及时输出；官方基础镜像静态审计找到固定双帧线程的管线等待路径。该候选维持不可生产选择，推进 FFmpeg 后备的独立验收；不能调用私有强制排空入口或修改系统库，也不把此限制推广到其他设备。具体证据与版本边界见[系统解码验证](../validation/harmony-system-codec-20261013.md)。

官方资料：[AVCodec 支持格式](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/media/avcodec/avcodec-support-formats.md)、[视频解码](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/media/avcodec/video-decoding.md)。H.264/HEVC 能力不能替代 RemoteFX、Bitmap/RLE 或 Apple MVS 解码；RDP AVC444 组合与 H.264 High 4:4:4 码流需分别验证。HEVC 系统解码存在也不解除 RDP HEVC wire 门。Windows、macOS 和 Linux 保持各自平台组合根策略。 当前 registry 默认拒绝 Native + SoftwareExact；新增顺序必须通过显式、中立的选择策略由 OHOS 组合根启用，保留现有 `VideoDecoderRegistry::new` 默认行为及桌面回归，不能全局改变软件后端优先级。

## 交互与安全

手机为触控优先，登录表单自适应，远程视图支持平移/缩放与明确的输入模式；PC 保留系统标题栏及居中的会话操作。首次证书记录并连接，相同指纹继续，变化停止自动连接。输入只在前台、焦点归远程内容且当前 generation 完整基线已呈现时发送；失焦/后台/断开 ReleaseAll。

RDP 能力门保持现状：生产 LegacyOnly；实际 AVC/HEVC wire/profile/decoder/架构/包/互操作门未全通过不广告。软件热路径使用已有汇编/NEON；鸿蒙 native codec 接在既有 media 契约下。不可用音频、剪贴板及 codec 必须显示其真实状态。

HIG、Material 3 的可访问性、焦点和交互基线继续适用；具体窗口与控件遵循鸿蒙 ArkUI 平台能力，手机不复制桌面标题栏，PC 不复制手机导航。功能图标仍使用现有 Material Symbols Rounded 资源和许可证。验证明暗主题、字体缩放、触控目标、横竖屏、IME 与 PC 窗口缩放。

PC 标题栏采用[华为官方自由多窗沉浸式方案](https://developer.huawei.com/consumer/cn/doc/doccenter-dev-faq/faqs-arkui-1089)：`setWindowDecorVisible(false)` 隐藏默认标题区但保留系统三键，`setWindowDecorHeight` 设置高度，`getTitleButtonRect` 与 `windowTitleButtonRectChange` 驱动避让。产品会话控件在原标题区按窗口几何中心排列；远程内容从该区下缘开始，渲染与输入使用同一矩形。不得在系统标题栏下另加工具栏，也不修改系统 ArkUI 的 titlebar 构建文件。正常权限 API 可用性、系统三键、拖动、双击、窗口缩放、全屏和窄宽折叠仍需实际 GUI 验证；接口文档不是运行通过的证据。

2026-10-13 设计资料核对：[Apple HIG Windows](https://developer.apple.com/design/human-interface-guidelines/windows) 要求适应窗口大小并保留系统窗口行为；[Material 3 Text fields](https://m3.material.io/components/text-fields/guidelines) 与其[官方组件文档](https://github.com/material-components/material-web/blob/main/docs/components/text-field.md) 为标签、支持文字和错误语义的基线；[华为多设备适配指南](https://developer.huawei.com/consumer/cn/multidevice/adaptive-apps/) 要求布局和交互随窗口空间动态响应。M3 主站在当前网络下正文不可读取，具体 GUI 实施前仍需核对可访问的官方组件资料。没有批准省略持久标签、焦点、触控尺寸或系统窗口行为的例外。

## 当前工具链证据

本机 DevEco Studio 26.0.0，SDK API 26 / 26.0.0.105，Hvigor 6.26.4，ohpm 26.0.0.630，Node 24.14.1。PATH 的 deveco 0.2.1 是 AI agent CLI；构建实际调用 Studio 的 Hvigor、OHPM、CMake/Clang 和 HDC。

已有的 Pura 90 Pro ARM64 实例保留不变。已通过官方 CLI 下载 2in1 ARM64 7.0.0.107 镜像，并创建、启动独立的手机/PC API 26 实例；两者 HDC Connected、boot complete、arm64-v8a。手机镜像包 7.0.0.106，PC 包 7.0.0.107，两个 guest 均自报 OpenHarmony-7.0.0.105。Rust 1.96 OHOS std 已安装，设计预检时现有 core/frame/app 的 OHOS target check 通过。后续 Tasks 1–3 已完成 HAP、图形、应用状态及 HUKS 的受限验证，见[应用与安全存储验证](../validation/harmony-app-services-20261013.md)；真实协议互操作仍未完成。详见[环境验证](../validation/harmony-environment-20261013.md)。

## 保留的未决门

被控鸿蒙手机的系统原生远程服务尚未确认；不会以自建 server、HDC、修改系统权限或注入服务替代。Mac/Windows 授权目标的地址和密码从本机安全提供者读取，不进入工程。HAP 签名若需要用户账号认证，保留已构建产物并通过正常开发签名流程完成，不伪造系统权限。


## 手机横屏优先与自动旋转（2026-10-15 用户要求）

手机窗口首次创建时使用官方`window.Orientation.USER_ROTATION_LANDSCAPE`：
先横屏显示，随后按传感器和系统旋转锁定切换方向。该API从12起提供，本工程
最低API26；不使用只允许横屏/反向横屏的`AUTO_ROTATION_LANDSCAPE`系列。
仅`deviceInfo.deviceType === 'phone'`分支设置，保留共享manifest的原策略与
PC原生自由窗口行为；不改Windows/macOS/Linux壳或协议/解码器。策略只在窗口
创建时设置，不在前台恢复或每次resize时强制重设，避免把用户转回竖屏的选择覆盖。
失败只记录既有窗口错误，不中断加载。旋转继续走原有安全区、geometry及焦点释放。

按[华为横竖屏开发指南](https://developer.huawei.com/consumer/cn/doc/best-practices/bpta-landscape-and-portrait-development)、本机正式SDK Orientation枚举，结合
[Apple HIG Layout](https://developer.apple.com/design/human-interface-guidelines/layout)与
[Material官方自适应布局](https://github.com/material-components/material-components-android/blob/master/docs/adaptive/CanonicalLayouts.md)核对。
M3主站的相关布局路径仍不可直接读取，以官方组件库的自适应资料补充；本项不改变
控件/信息架构，不增加GUI基线例外。验收分别记录实际HAP构建、手机启动默认横屏、
自动切回竖屏及再次横屏、安全区/表单输入保持；PC新包实际执行若未进行必须明确。


首次手机实测确认横屏策略生效，但旧单列表单超出横屏首屏，因此手机宽而矮的
内容区（宽≥600vp且高<500vp）采用两行双列字段：目标/端口与地址、用户名与
密码；标题与最近连接、保存选项与连接按钮分别并排。普通字号完整显示主要
登录信息，错误、大字或键盘弹出仍可滚动；PC及手机竖屏保持既有布局。字段
复用同一状态、验证和安全密码控件，不改变48vp触控目标、Enter/IME和凭据边界。


## 普通应用标准 MVS 的实验接线（2026-10-15）

按既有 Task 5 先用默认关闭的 `normal-hap-apple-mvs-validation` 接入已完成
ARM64 正确性检查的 Apple 标准 MVS，可与 RDP 验证开关共同构建。可选依赖及
原生内核严格限定 OHOS ARM64；组合根累加协议，复用原 AppEngine/SessionHost。
共享 Mac Automatic 仍选择 HP；标准 MVS 必须由正常表单明确选择，不能暗中回落。
第一阶段仅实现组合与实际最终 FFI，随后审查并串行实现模式选择、同包 HAP 和
手机/PC 真实 Mac 会话。保留音频/video loader 不可用及当前键盘/提交文本门；
不把首个指针实验称作完整客户端。性能资格单独推进，不阻止明确默认关闭的
实验接线，也不能由实验成功自动开放生产。原输入能力与提交文本分离另列任务。
