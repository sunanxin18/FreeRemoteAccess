# macOS 产品包

> 2026-09-08 用户调整构建范围：macOS 仅构建 ARM64；Intel/x86_64 不支持当前构建与发布，不再是交付门禁。此前 Intel/Rosetta 结果仅保留为历史证据。Windows/Linux 的 x86、x86_64、ARM64 范围不变。

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
可通过 `FRD_MACOS_ARCH=arm64`（或规范化别名 `aarch64`）显式选择目标架构；Intel/x86_64 请求在构建前拒绝。
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

可选 FFmpeg bundle 由 `tools/build-ffmpeg-macos.sh` 构建，仅支持 ARM64 AArch64/NEON；打包脚本将三库置于
`Contents/MacOS/codecs/ffmpeg-8-1-2/macos-aarch64`，许可证与来源说明放入
`Contents/Resources/codecs/ffmpeg-8-1-2`。macOS 代码签名把代码目录中的带点
目录名解释为 bundle，因此固定版本目录使用连字符；Windows 布局不变。
插件加载要求完整包通过 `codesign --verify --deep --strict`，并拒绝链接或包外路径。
验证脚本会调用隐藏的 `--verify-codec-bundle` 检查实际 dylib 加载及 ABI。

固定来源默认仍为 `upstream`，保持原五文件 bundle。显式修改配方的构建与 staging 使用相同选择：

```sh
FRD_FFMPEG_SOURCE_RECIPE=frd-h264dc-aarch64-v1 bash tools/build-ffmpeg-macos.sh
FRD_FFMPEG_SOURCE_RECIPE=frd-h264dc-aarch64-v1 bash tools/stage-macos-package.sh release
```

`FRD_FFMPEG_ARCHIVE` 可指定已有固定 SHA256 的原归档；默认在 build root 下载。
修改配方只重放仓库固定 patch，完整检查源码树 pre/postimage 和未改 C oracle。
对应源码 ZIP 包含原归档、LGPL、固定 patch/recipe、共享与 macOS helper、此平台 builder 和重建说明；
不得以旧 `changes.diff` 的无修改声明代替。ZIP、真实修改说明和 provenance 放入 Resources，
三 dylib 继续保持既有加载路径；分发时必须将同一 ZIP 放在应用包旁的同一下载页面。
原归档身份只认证原归档，不能认证本地修改，未声称本地 patch 是 upstream 签名发布。

modified bundle 使用独立 `freeremotedesk.macos.ffmpeg-bundle.v1` 来源契约，记录 profile、完整固定 source metadata、
实际 configure/编译器/输入/产物 SHA 和数学核身份。从同源未剥离库实际符号定位固定276字节，
在 install_name 调整及签名后、staging 重签后再次核对可执行 section 和数学代码 SHA。
签名外 payload 身份仅排除签名 blob 及对应 LINKEDIT 长度；其余 bytes 必须保持。
这些源码与包装门不代表完整 codec 数值、无 EOS 出帧、GUI、真实远程互操作或目标性能验收；
Linux/Windows 配方接线仍由各自独立任务处理，macOS Intel 不支持。
