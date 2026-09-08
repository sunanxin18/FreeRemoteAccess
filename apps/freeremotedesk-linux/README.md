# FreeRemoteDesk Linux 应用组合根

面向 Linux i686、x86_64 和 AArch64 的独立客户端入口。Linux 默认构建使用 GTK4
Application + ApplicationWindow 生命周期、HeaderBar/GLArea 原生壳；应用 id 为
`com.sunanxin18.freeremotedesk`，与 Linux desktop entry 的 `freeremotedesk` 窗口身份对应。
`--no-default-features` 只用于离线 winit fixture。非 Linux 或其他架构在
创建事件循环、访问平台存储和加载 codec 前明确返回 `linux_client_platform_unsupported`。
`--help` 可以离线查看，不代表当前宿主支持运行客户端。

复用共享登录表单、会话、RDP/Apple 协议和桌面 shell，通过 `frd-platform-linux`
接入 XDG 非秘密配置、TOFU 证书指纹和 Secret Service 凭据存储。密码只能由安全
输入框或非回显 provider 提供，没有明文密码命令行参数。

默认 RDP 使用 LegacyOnly，且不加载实验 FFmpeg backend。显式
`--rdp-egfx-experiment avc420` 或 `avc444` 必须成功加载当前可执行文件旁的受信
codec bundle 并通过对应 profile 能力检查，才使用 ValidationOnly；失败明确退出，
不会静默改用另一实验模式。HEVC 没有开放实验选项或生产互操作声明。

`--verify-codec-bundle` 在 Linux 支持架构加载实际随包 backend，用于完整包验证；
所需布局为 `可执行文件目录/codecs/ffmpeg-8.1.2/linux-{x86,x86_64,aarch64}`。
加载器还检查权限、所有者和链接，不能用系统 FFmpeg 或任意搜索路径替代。

Linux 原生音频输出尚未接线，因此能力、产品策略和音频工厂都明确不可用。
桌面图标由 desktop entry/hicolor 包资源提供。CI 的 Linux GTK adapter 会在三种
目标架构编译正式组合根，并在隔离 X11 中通过 `tools/verify-linux-product-x11.sh`
核验窗口映射、WM_CLASS、焦点、XTEST 鼠标/键盘事件和干净关闭；同一轮的 X11/Wayland
GLArea fixture 核验真实 GL 上下文与帧事务。Mesa 软件驱动和合成事件不等于硬件 GPU
或真实 RDP 控制证据；Wayland 物理输入和真实服务器连接仍需单列验收。
