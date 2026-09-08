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
系统运行依赖包括 glibc、libgcc、libstdc++ 和 zlib（libz.so.1）；完整依赖由包验证器检查。
窗口运行需要 X11 或 Wayland 会话与 Vulkan/GLES 驱动。字体只在当前窗口树的私有
Pango 配置中注册，不写入系统或用户字体目录。Linux 音频尚未接入。

运行 `./freeremotedesk-linux --verify-codec-bundle` 验证实际解码器加载；该命令不连接
远程服务器，不代表 GUI、远程输入、AVC 协商或 HEVC 互操作验收已通过。
默认 RDP 图形模式为 LegacyOnly，现代图形仍须显式选择实验模式。
