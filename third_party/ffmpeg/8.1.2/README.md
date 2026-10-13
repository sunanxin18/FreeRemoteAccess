# FFmpeg 8.1.2 LGPL Windows 构建

FreeRemoteDesk 的可选软件解码插件只支持固定的 FFmpeg 8.1.2（`libavcodec` 62）。发布源码与签名来自：

- `https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz`
- `https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz.asc`
- `https://ffmpeg.org/ffmpeg-devel.asc`

源码归档 SHA-256 为 `464BEB5E7BF0C311E68B45AE2F04E9CC2AF88851ABB4082231742A74D97B524C`。构建脚本还要求 GPG `VALIDSIG` 的主密钥指纹严格等于 `FCF986EA15E6E293A5644F10B4322F04D67658D8`。签名文件和公钥文件的 SHA-256 分别为 `0A0963FCCD70597838073F3E31B20F4A4D8CC2B5E577472C9A5A1F22624246F8` 与 `397B3BECEDCD5A98769967FF1FF8501DDC89F8368B8F766E4701377D7DBAABE5`。

在仓库根目录运行：

```powershell
pwsh -NoProfile -File tools/build-ffmpeg-windows.ps1 -Configuration Release
```

脚本使用指定的 WSL distribution 中已有的 Bash、GNU Make 和 MinGW-w64 GCC 构建共享库，再用 Visual Studio 2022 x64 工具生成 MSVC import libraries 与插件。完整参数记录在 `configure-windows.txt`。每次普通构建都会重新校验归档与签名、从已验证归档解压到唯一的新目录并重新 configure/build；不会复用旧解压树、build tree 或 configure cache。配置明确关闭 GPL、nonfree、version3、网络、自动外部库探测和静态库，只启用 HEVC decoder/parser 与 file protocol；不链接 x264、x265、fdk-aac 或其他外部 codec 库。

运行时 bundle 以空 staging 目录构造，只允许 `avcodec-62.dll`、`avutil-60.dll` 和 `freeremotedesk_ffmpeg.dll` 三个文件；脚本验证三个 PE 的精确 imports 后，以同卷目录重命名方式可恢复地替换旧 bundle。它是动态 LGPL 依赖，不会把 FFmpeg 链入默认 GUI/workspace 构建。

FreeRemoteDesk 对应源码的固定分发件名称为 `FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip`。脚本在被忽略的 `.codex-target/ffmpeg-8.1.2/release-assets/` 中生成并验证这个 staging asset；它精确包含已签名的 `ffmpeg-8.1.2.tar.xz`、detached signature、`LICENSE.LGPLv2.1`、`changes.diff` 和带 hash/发布位置的 `SOURCE-MANIFEST.txt`。这个本地 staging 目录本身不是对外分发位置。Task 9 打包时，分发者必须在其控制的、与每个 FreeRemoteDesk Windows binary package 相同的 release/download 页面把该 ZIP 作为 sibling asset 发布，并让二进制 package/installer 明确指向该固定 asset；不能只依赖 upstream URL 或开发机 `.codex-target`。不要提交大型源码归档、DLL 或 build tree。

从一台没有预设 `FFMPEG_DIR`、Visual Studio 环境变量或 FFmpeg runtime `PATH` 的新 PowerShell 自举构建并运行 native 6 项与 fixture 2 项测试：

```powershell
pwsh -NoProfile -File tools/build-ffmpeg-windows.ps1 -Configuration Release -RunNativeTests
```

构建安全回归使用 workspace 中已有的 Pester：

```powershell
pwsh -NoProfile -Command "Import-Module Pester -Force; Invoke-Pester -Script 'tools\tests\ffmpeg-build-common.Tests.ps1' -EnableExit"
```

本目录保存 FFmpeg 原始 `LICENSE.LGPLv2.1`；`changes.diff` 记录源码未修改。

该构建仅建立 CPU software HEVC Main444 8-bit 精确解码，不声明任何 FFmpeg hardware exact 路径。

## 固定 AArch64 H.264 luma DC 修改配方

OHOS builder 现在显式接受 `--source-recipe upstream`（默认，保留原 schema 1）
或 `--source-recipe frd-h264dc-aarch64-v1`（schema 2）。后者仅支持 AArch64，
使用 `h264-luma-dc-aarch64-recipe.json` 与
`patches/0001-aarch64-h264-luma-dc-neon.patch` 固定的两文件修改；
`tools/ffmpeg-source-recipe.py` 固定 pin/patch hash，验证完整归档、preimage、
无 fuzz/offset 的 fresh 重放、postimage 及所有其他文件未改变。不能传入任意 patch。
此来源身份与每个 bundle 的输入记录和真实 ELF 可执行核字节相互核对。
原 `changes.diff` 继续仅说明 upstream 配方；修改配方没有“源码未修改”标签。

```sh
tools/build-ffmpeg-ohos.sh --archive /path/to/ffmpeg-8.1.2.tar.xz \
  --source-recipe frd-h264dc-aarch64-v1
```

修改包附带 `FFmpeg-CHANGES.diff` 和 `FFmpeg-SOURCE-RECIPE.json`。同一 unique
build attempt 还生成并逐字节验证
`FreeRemoteDesk-ffmpeg-8.1.2-frd-h264dc-aarch64-v1-corresponding-source.zip`，
包含完整 base archive、许可、固定 patch/pin、builder/helper 和 source manifest。
分发者必须将此 ZIP 与 binary 放在同一 release/download 页面；本地生成不表示已发布。
原归档保持原 hash；local patch 没有上游发布签名。修改来源不能误指仅含原样源码的 ZIP。

从对应源码 ZIP 解压后，可用其中 helper 单独重建修改树：

```sh
python3 tools/ffmpeg-source-recipe.py --archive ffmpeg-8.1.2.tar.xz \
  --destination fresh-source --source-recipe frd-h264dc-aarch64-v1
```

OHOS 构建还需要匹配的完整 FreeRemoteDesk 源码、官方 API26 / 26.0.0.105 SDK
和 coherent Rust 1.96.0。SDK、configure、codec、线程、plugin ABI 保持原合同。
HAP 仍为固定六个 native 库；新增的是来源材料。此配方与静态 ELF/HAP 验证
不能代替目标进程的 dispatch、像素、性能或 live interoperability 验收。
桌面 builders 尚未消费这个修改配方，不宣称其新核已验证。
