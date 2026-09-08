# FreeRemoteDesk Linux 应用组合根

面向 Linux i686、x86_64 和 AArch64 的独立客户端入口。非 Linux 或其他架构在
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
桌面图标由后续 desktop entry/hicolor 包资源提供。当前组合根及离线测试不代表
X11/Wayland GUI、原生 Secret Service 或真实 RDP 控制已经验收；完整包与原生
窗口验证继续按 Linux 客户端计划完成。
