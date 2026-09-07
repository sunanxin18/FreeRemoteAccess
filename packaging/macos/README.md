# macOS 产品包

macOS 产品入口为 `freeremotedesk-macos`，复用共享桌面界面、会话控制器和协议适配器；
窗口由 AppKit/winit 管理，图形使用 Metal。Windows 目标使用 IronRDP，Mac 目标注册
已有 Apple RFB 和 HPSS 协议。注册协议不代表所有编解码器或目标组合已有实机证明。

```sh
./tools/stage-macos-package.sh debug
open target/macos/debug/FreeRemoteDesk.app
./tools/stage-macos-package.sh release
```

需要 macOS、Xcode Command Line Tools 和支持项目依赖的 Rust 工具链。
产物默认是当前构建机架构的 `.app`，最低系统版本为 macOS 12.0；并非 universal binary。
可通过 `FRD_MACOS_ARCH=arm64` 或 `FRD_MACOS_ARCH=x86_64` 显式选择目标架构；跨架构构建还需要对应的 Rust target、Apple SDK 和 Clang 交叉编译能力。
图标从 `assets/app-icon/apple` 的共同产品图层生成，无预制圆角遮罩。
脚本执行本机 ad-hoc 签名，不执行 Developer ID 签名或 Apple 公证。

正常启动显示连接表单。CLI 可预填 `--target windows --address HOST --port 3389 --protocol rdp`，
使用 `--username-provider environment --password-provider environment --connect` 自动连接；
账号和密码由 `FRD_USERNAME`、`FRD_PASSWORD` 环境提供器读取，CLI 不接受明文密码参数。
请通过非回显输入或安全凭据启动器注入变量，不将密码写进 shell 命令或日志。

离线验收可直接运行包中可执行文件，并传入 `--test-texture
--test-texture-resize-after-ms 500 --test-texture-exit-after-ms 1500`。
`verify-macos-package.sh` 检查结构、签名、依赖和帮助输出，不替代 GUI、键鼠和实机验收。
密码由 macOS Keychain 保存；连接元数据与证书指纹使用当前用户 Application Support 目录。

可选 FFmpeg bundle 由 `tools/build-ffmpeg-macos.sh` 构建，脚本支持 `x86_64` 的 NASM/x86asm 与 `arm64` 的 AArch64/NEON 路径；打包脚本将三库置于
`Contents/MacOS/codecs/ffmpeg-8-1-2/<architecture>`，许可证与来源说明放入
`Contents/Resources/codecs/ffmpeg-8-1-2`。macOS 代码签名把代码目录中的带点
目录名解释为 bundle，因此固定版本目录使用连字符；Windows 布局不变。
插件加载要求完整包通过 `codesign --verify --deep --strict`，并拒绝链接或包外路径。
验证脚本会调用隐藏的 `--verify-codec-bundle` 检查实际 dylib 加载及 ABI。
