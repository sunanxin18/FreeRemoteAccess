# FreeRemoteDesk Linux 完整客户端包

本包包含实际 Linux 客户端、固定版本 FFmpeg 解码器、随包 Noto Sans SC 中文回退字体、
desktop entry、hicolor 图标和许可证。支持 i686、x86_64、AArch64 的独立构建；不同架构的包不能混用。

在解压目录直接运行 `./freeremotedesk-linux`。桌面菜单集成时，将整个包保留在同一
可信安装目录，将该目录加入 PATH，并把 `share/applications/freeremotedesk.desktop`
和 `share/icons/hicolor` 中的文件安装到宿主标准 XDG 应用及图标目录。
不要把 ELF 单独复制到 `/usr/bin`：解码器必须始终位于可执行文件旁的
`codecs/ffmpeg-8.1.2/linux-{x86,x86_64,aarch64}` 目录。

安装目录和文件必须属于同一个安装所有者，不得允许组或其他用户写入，也不得包含
符号链接。密码只使用客户端宿主的 Secret Service；不可用时不会退回普通配置文件。
系统运行依赖包括 glibc、libgcc、libstdc++、zlib（libz.so.1）和 GTK 4.14 运行库；
GTK 壳的 `libgtk-4.so.1`、`libgdk-4.so.1`、Pango/Cairo、Graphene、Epoxy 等固定
SONAME 由包验证器逐项检查。窗口运行需要 X11 或 Wayland 会话与 Vulkan/GLES 驱动。
正式入口使用 GTK ApplicationWindow（app-id `com.sunanxin18.freeremotedesk`）；窗口
装饰由宿主 compositor 管理，产品控件保持在原生 HeaderBar，远程内容只占据标题栏下方
的 GLArea。隔离 X11 产品 smoke 可运行：
`tools/verify-linux-product-x11.sh /绝对路径/freeremotedesk-linux`。
Wayland compositor 已提供私有 `WAYLAND_DISPLAY` 时可运行
`tools/verify-linux-product-wayland.sh /绝对路径/freeremotedesk-linux`。
这些 smoke 只验证窗口生命周期、WM_CLASS/app-id、焦点和 XTEST（仅 X11）事件，
不连接远程服务器；Wayland 全局物理输入不能由客户端脚本注入。
字体只在当前窗口树的私有
Pango 配置中注册，不写入系统或用户字体目录。Linux 音频尚未接入。

运行 `./freeremotedesk-linux --verify-codec-bundle` 验证实际解码器加载；该命令不连接
远程服务器，不代表 GUI、远程输入、AVC 协商或 HEVC 互操作验收已通过。
默认 RDP 图形模式为 LegacyOnly，现代图形仍须显式选择实验模式。

Linux FFmpeg 默认来源配方为 `upstream`，三个目标各自保留原版源码；只有显式
`FRD_FFMPEG_SOURCE_RECIPE=frd-h264dc-aarch64-v1` 与 AArch64 目标组合允许本地固定
H264 luma DC NEON 修改。未知配方或修改配方与 x86/AMD64 组合会在下载/创建源码前拒绝。
修改包 codec 目录包含固定 patch、recipe、Linux provenance 和完整对应源码 ZIP；
必须与匹配二进制一起发布。原归档摘要只认证原版源码，不认证本地修改。
构建器可用 `FRD_FFMPEG_ARCHIVE` 选择已缓存的固定归档；成功构建时显式
`FRD_FFMPEG_KEEP_BUILD=1` 保留实际配置树与同源对象，供数值、分派和基准验证。
交叉构建的 ELF/来源检查不代表 Linux ARM64 执行、完整 codec 资格或远程互操作通过。

显式修改配方的原生 Linux ARM64 数值资格检查使用
`FRD_FFMPEG_SOURCE_RECIPE=frd-h264dc-aarch64-v1 bash tools/qualify-linux-h264-recipe.sh`。
默认要求 `target/ffmpeg-linux` 尚不存在；也可用 `--build-root /绝对路径/新的隔离目录`
和 `--archive /绝对路径/固定归档`。该门只调用一次既有 builder，开启其目标插件夹具，
保留实际配置树、对象及安装 prefix，并使用私有 Cargo 输出；失败不会回退 upstream。
它复用既有 C oracle/数值 harness 检查同 shipping 对象的数值、30 种分派、内存保护与
ARM64 ABI，再以同一 executable 在编译结束后执行三个独立有界基准窗口。
`runtime-evidence/qualification.json`、原数值 `numeric/results.json`、全部命令日志、
三个基准 JSON/日志和前后输入摘要属于运行证据；CI 会在失败时保留诊断产物。
须检查资格步骤及最终包 smoke 均成功，上传本身不代表成功。这只覆盖单个 H264
luma DC 核及该 CI 主机测量；本地模拟契约、小型 ELF、完整 codec、GUI 与远程互操作
属于不同验证层级。
工作流的手动 `target_architecture=aarch64` 与
`arm64_source_recipe=frd-h264dc-aarch64-v1` 只运行固定原生 ARM64 行；默认 `all`、
push 和 PR 继续三个原有 profile。使用自选隔离根后，后续 package staging 必须显式
设置相同 `FRD_FFMPEG_BUILD_ROOT`，从已资格检查的 bundle 取材。
