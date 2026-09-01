# FFmpeg 8.1.2 LGPL Windows 构建

FreeRemoteDesk 的可选软件解码插件只支持固定的 FFmpeg 8.1.2（`libavcodec` 62）。发布源码与签名来自：

- `https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz`
- `https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz.asc`
- `https://ffmpeg.org/ffmpeg-devel.asc`

源码归档 SHA-256 为 `464BEB5E7BF0C311E68B45AE2F04E9CC2AF88851ABB4082231742A74D97B524C`。构建脚本还要求 GPG `VALIDSIG` 的主密钥指纹严格等于 `FCF986EA15E6E293A5644F10B4322F04D67658D8`。签名文件和公钥文件的 SHA-256 分别为 `0A0963FCCD70597838073F3E31B20F4A4D8CC2B5E577472C9A5A1F22624246F8` 与 `397B3BECEDCD5A98769967FF1FF8501DDC89F8368B8F766E4701377D7DBAABE5`。

在仓库根目录运行：

```powershell
pwsh -File tools/build-ffmpeg-windows.ps1 -Configuration Release
```

脚本使用指定的 WSL distribution 中已有的 Bash、GNU Make 和 MinGW-w64 GCC 构建共享库，再用 Visual Studio 2022 x64 工具生成 MSVC import libraries 与插件。完整参数记录在 `configure-windows.txt`。配置明确关闭 GPL、nonfree、version3、网络、自动外部库探测和静态库，只启用 HEVC decoder/parser 与 file protocol；不链接 x264、x265、fdk-aac 或其他外部 codec 库。运行时 bundle 只复制 `avcodec-62.dll`、`avutil-60.dll` 和插件，因此是动态 LGPL 依赖，不会把 FFmpeg 链入默认 GUI/workspace 构建。

已验证源码归档、解压源码、构建树、对应源代码分发副本和 DLL 都保留在被忽略的 `.codex-target/ffmpeg-8.1.2/` 下，不提交二进制或构建树。分发 DLL 时，必须同时按 LGPL v2.1-or-later 的要求提供上述原始源码 URL，或提供该确切已验证源码归档的对应源代码副本。本目录保存 FFmpeg 原始 `COPYING.LGPLv2.1`；`changes.diff` 记录源码未修改。

该构建仅建立 CPU software HEVC Main444 8-bit 精确解码，不声明任何 FFmpeg hardware exact 路径。
