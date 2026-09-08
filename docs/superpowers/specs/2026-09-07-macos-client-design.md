# macOS 产品客户端

用户于 2026-09-07 要求完整实现 macOS 客户端，不能以无 GUI 协议探针代替产品。
本次交付独立 `freeremotedesk-macos` 应用，复用现有分层，不把 Windows 平台实现
作为 macOS 的运行依赖。

## 架构

- `apps/freeremotedesk-macos` 是 composition root，显式注入 Macintosh RDP 身份，
  注册现有 Apple 与 RDP adapter，使用共享登录表单、会话控制器和桌面 shell。
- `frd-platform-macos` 实现 Application Support 配置/指纹、Keychain 密码、
  单实例生命周期锁和 CoreAudio 输出。账号密码不经 argv、普通文件或日志。
  凭据 stage 仅驻内存，认证成功后 commit 到 Keychain，取消或退出丢弃。
- `frd-shell-desktop` 在 macOS 使用 Metal。原生交通灯、拖动、全屏和窗口管理
  保留 macOS 行为；产品控制居中，远程画面和指针映射使用相同的标题栏下方矩形。
- FFmpeg 8.1.2 HEVC 软件解码器可通过固定源码构建后随包携带，使用应用包内
  固定绝对路径，不查找 Homebrew 或当前工作目录中的动态库。包验证和运行时
  加载保留代码签名及 ABI 验证，代码签名完整性不冒充 Developer ID 公证。

## 产品行为

连接表单与 Windows 保持相同信息层次，保留标签、密码安全输入、Enter 单次提交、
错误后保留输入、键盘导航与中文错误说明。RDP 首次指纹自动记录，后续变化停止
自动连接。会话状态、证书详情、取消和断开位于标题栏；不以工具栏占用远程内容。

原生 macOS 窗口使用宿主缩放和外观，保留 native traffic lights 与菜单/系统快捷键。
图标来自现有 Apple 未蒙版源图层，导出原生多尺寸 ICNS。

## 交付与验收

`tools/stage-macos-package.sh` 生成 `.app`，包含 Info.plist、ICNS、可执行文件和
可用的固定 codec bundle，逐个签署嵌套代码再签署应用。分发前验证由独立脚本执行。
本机 ad-hoc 签名包可运行；Developer ID、公证、Intel 实机证据必须单独记录。

验收必须启动真实 `.app`，检查登录页面、原生窗口控制、键盘焦点、密码 Enter、
RDP 首次连接与已存指纹重连、远程画面、无破坏性的鼠标/键盘操作、断开和进程清理。
Keychain 用隔离的合成凭据验证；不为测试保存用户真实密码。
客户端交付与具体服务端协议功能分别记录，不能把 RDP GUI 通过扩展为 Apple HP、
音频或其他未执行的真机测试通过。

## 2026-09-08 控制岛隐藏与首次窗口几何修正

macOS ARM64 会话复用共享控制岛隐藏状态机，不在每次重绘时强制显示。
隐藏只影响产品控件；原生交通灯和标题栏安全区域继续保留。显隐前后的远程内容
矩形完全相同，渲染与输入继续使用同一矩形，不因悬停而缩放或移动远程桌面。

首次完整远程帧到达后，平台 shell 根据该画面的实际比例、固定有效标题栏高度和
当前屏幕的原生可用区域计算窗口尺寸。协议请求仍优先本地显示器原生分辨率，不
降为固定 2560×1440，也不通过拉伸像素消除黑边。后续手动缩放和重连保留用户尺寸；
任意比例的最大化/全屏窗口仍可能出现等比显示的留边，须由已验证的动态分辨率能力
另行解决，不能冒充解码失败或偷偷裁剪画面。

本次复核官方 [Apple Windows HIG](https://developer.apple.com/design/human-interface-guidelines/windows)、
[AppKit FullSizeContentView](https://developer.apple.com/documentation/appkit/nswindow/stylemask-swift.struct/fullsizecontentview)
与 [Material 3 Toolbars](https://m3.material.io/components/toolbars/overview)。
保留宿主窗口控制、拖动/缩放和原生安全区域；居中产品控件自动隐藏是既有远程内容
优先策略，隐藏后仍保留顶部唤出路径和键盘可达性。本次不新增视觉规范例外。

本条覆盖早期 Intel 交付说明：macOS 当前及后续仅支持 ARM64；Intel 不再是构建、
发布或验收目标。真实用户已授权的登录信息持久化使用 Keychain，不使用普通配置。
