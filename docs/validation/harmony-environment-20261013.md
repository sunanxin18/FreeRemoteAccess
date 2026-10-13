# 鸿蒙客户端开发环境验证

日期：2026-10-13；宿主 macOS ARM64。用户授权自动下载缺失的官方模拟器镜像。

## 完成范围

官方 DevEco Emulator CLI 成功下载并解压缺失的 PC/2in1 ARM64 镜像：2,743,949,835 bytes，镜像包 7.0.0.107。手机复用已有 7.0.0.106 镜像。新建了两个独立测试实例，原有 `Pura 90 Pro` 实例未修改。

| 独立实例 | 镜像包 | 显示配置 | HDC | 启动与架构 |
|---|---|---|---|---|
| FreeRemoteDesk Phone API26 | phone_all_arm / 7.0.0.106 | 1256×2760，560 dpi | 127.0.0.1:5555，Connected | `boot.completed=true`，arm64-v8a |
| FreeRemoteDesk PC API26 | pc_all_arm / 7.0.0.107 | 3120×2080，304 dpi | 127.0.0.1:5557，Connected | `boot.completed=true`，arm64-v8a |

两台 guest 自报 `OpenHarmony-7.0.0.105`，与镜像包版本是不同观测。以上为本地模拟器启动证据，不等于商用 HarmonyOS 真机或 FreeRemoteDesk HAP 运行证据。没有安装 HAP、操作远程 Windows/Mac 会话或改动模拟器权限。

工具版本：DevEco Studio 26.0.0、SDK API 26 / 26.0.0.105、Emulator 26.0.0.400、Hvigor 6.26.4、OHPM 26.0.0.630、Node 24.14.1、OHOS Clang 15.0.4、CMake 3.28.2。PATH 中的 `deveco 0.2.1` 是 AI agent CLI，实际 HAP 构建需用 Studio 提供的 Hvigor/OHPM/Native 工具链。

## 源码基线与交叉检查

安装 Rust 1.96.0 的 `aarch64-unknown-linux-ohos` 标准库成功，之后在未改动产品源码的基线上执行：

```sh
cargo test --locked -p frd-shell-desktop -p frd-session
cargo check --locked --target aarch64-unknown-linux-ohos \
  -p frd-core -p frd-frame -p frd-app
```

- 宿主测试：216 项 desktop、9 项 session、2 项集成测试和 1 项文档测试通过。
- OHOS target check：core、frame、app 以及 platform-api、media-api、protocol-api、session、ui-model 依赖检查通过。
- 未检查完整协议 native 依赖、共享 runtime、FFmpeg bundle、HAP 链接或图形呈现；尚无 OHOS 客户端真实远程登录证据。

## 共享运行时提取（655785d）

`frd-shell-runtime` 已提取原 session host、cleanup、video worker、rate fallback 和中立计数值。runtime 显式接收 decoder registry loader；桌面薄组合层保留原三参数构造器并注入原 FFmpeg loader。host/OHOS 的 runtime normal dependency tree 只包含五个中立 workspace crate，不包含 winit、egui、GTK、wgpu、FFmpeg 或平台 loader。独立任务审查通过，没有未解决的审查项。

- 迁移后 macOS ARM64 host：runtime 49、desktop 168、session 9、integration 2、doctest 1 通过；原 desktop 216 项拆为 runtime 原 48 项和 desktop 168 项，另新增一个生产注入/wake 用例。
- `tools/build-harmony.sh --check`：OHOS core/frame/app/runtime 通过。
- `tools/build-harmony.sh -p frd-compositor-wgpu`：现有 compositor/render-wgpu 的 OHOS target check 通过，尚未证明最终链接或 EGL 运行。
- `cargo check --locked --target x86_64-pc-windows-msvc -p frd-shell-desktop`：通过；未修改的 FFmpeg loader 有两个已有的 unused-function warning，未隐藏或放宽检查。
- `cargo check --locked -p frd-shell-runtime --target <target>`：i686/aarch64 Windows MSVC 与 i686/x86_64/aarch64 Linux GNU 全通过。Windows x64 与 macOS ARM64 由上述依赖构建覆盖。未构建 macOS Intel。
- 生产 desktop check（不启用 test-support）、受影响 crate 的 fmt check、shell syntax、缺少 SDK 和含空格 SDK 路径参数检查通过。

以上 Windows/Linux 结果是交叉编译检查，没有在这些系统上启动客户端。helper 当前使用 Unix host 的 SDK 布局，Windows host 构建鸿蒙包尚未验证。HAP、窗口生命周期和真实协议验收仍待后续任务。

## 本机原始环境记录

运行产生的无凭据记录保存在 gitignored 的 `target/harmony-validation/`：

- `environment-20261013.json`：SDK 与工具版本、手机镜像 metadata。
- `emulators-20261013.json`：镜像与实例清单、HDC 连接、两台 guest 的 boot/架构/版本、Rust installed target。
- `setup-20261013.md`：下载、创建、启动和 Rust target 安装命令及结果。

SDK 镜像位于本机配置的 `Library/Huawei/Sdk/system-image/HarmonyOS-7.0.0/`，未纳入 Git。HDC 端口属于这次启动记录，重新启动后需重新列出设备，不能硬编码进构建或产品。

## 镜像复核与重新启动

同日复核官方 `Emulator -imageList -downloaded true`，PC/2in1 镜像仍为 7.0.0.107；手机本地镜像 metadata 为 7.0.0.106。两个 FreeRemoteDesk 专用实例此前均关闭，本轮已重新启动，HDC 5555/5557 均为 Connected，`bootevent.boot.completed=true`，架构仍为 arm64-v8a。原始记录：`target/harmony-validation/restart-20261013.json`。这一步不重复下载已有镜像，也不升级或改动原 Pura 90 Pro 实例。

宿主 Mac 最初锁屏，用户随后已手动解锁。CUA 已能读取 DevEco 的设备管理器，但不能按名称或完整路径选中 SDK 裸 Mach-O `Emulator` 进程；IDE 设备投屏列表也没有可选设备。已请求用户明确指定官方 Emulator CLI 作为替代界面验证通道。在该通道获准并实际检查以前，不能用 guest 启动参数或原生呈现日志替代视觉验收。

2026-10-13 用户随后明确授权官方 Emulator CLI 进行两个专用实例的截图、旋转、点击及前后台验证。已实际完成初步图形与生命周期检查，结果和未通过的手机方向项见 [HAP 图形验证](harmony-hap-20261013.md)。该替代只用于本地测试界面，不改变产品的客户端边界。

## 协议原生依赖实际编译检查补充

2026-10-13 15:44–15:45，在初版应用层提交 `c908733` 上，以独立 `CARGO_TARGET_DIR=target/harmony-protocol-preflight`、`CARGO_BUILD_JOBS=2` 运行官方 OHOS helper：

```sh
tools/build-harmony.sh --check -p frd-protocol-rdp -p frd-protocol-apple
```

显式选择已安装 OHOS 标准库的 Rust 1.96.0 ARM64 cargo/rustc，结果 exit 0，用时 1 分 10 秒。`aws-lc-sys 0.44.0` 和 `fdk-aac-sys 0.5.0` 实际完成原生依赖构建，两个协议 crate 检查通过。RDP 有四条既存 scalar reference 未使用警告，无编译错误；日志保存在本地忽略目录 `target/harmony-protocol-preflight-check.log`。

这只补齐当前 macOS ARM64 宿主到 OHOS ARM64 的依赖编译证据：尚未证明最终 cdylib 链接、HAP 加载、guest AAC/视频 fixture、解码 SIMD 合规或远程认证。fdk-aac 上游 build script 按宿主选择 C++ runtime 的跨宿主风险仍须在后续构建适配中处理，不能从本机编译成功推断所有宿主都支持。

## 后续媒体工具链预检（2026-10-13）

隔离目录中用官方 OHOS Clang/sysroot 对固定 FFmpeg 8.1.2 源码完成 configure、make、install。源码 SHA-256 为 `464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`；启用 H.264/HEVC decoder/parser 和 AArch64 NEON，仅生成 avcodec/avutil，未启用网络或 GPL/nonfree。FFmpeg 内部选择 Linux ELF 配置，但编译器和 sysroot 明确为 OHOS；没有重命名 GNU/Linux 库作为目标库。

两库 ELF Machine 为 AArch64，SONAME 分别为 `libavcodec.so.62` / `libavutil.so.60`；前者依赖后者及 `libc.so`，后者仅依赖 `libc.so`。候选构建未接入产品插件、HAP 或能力广告。原始参数、日志、ELF 报告和 SHA 在本地 `target/harmony-ffmpeg-preflight/`。

两台专用模拟器均实际运行未经修改的 FFmpeg 上游 `libavutil/tests/cpu`，报告 ARMv8、NEON、DOTPROD、SVE2、CRC 与 4 CPU。该程序静态链接 libavutil，仅证明目标测试执行和能力查询，不证明共享插件加载、扩展指令安全执行或任何解码输出。特别是报告含 SVE2 而无 SVE，不能凭此开放相应运行路径；后续必须以实际 codec fixture 验证。产品 builder、加载命名空间、C/Rust ABI、像素一致性及生命周期测试仍未完成。

## AVCodec Kit 系统库优先验证（2026-10-13）

用户要求支持时优先使用鸿蒙系统库。本机 API 26 SDK 提供 `native_avcapability.h`、`native_avcodec_videodecoder.h` 及 `libnative_media_vdec.so`、`libnative_media_codecbase.so`、`libnative_media_core.so`。官方支持表列出 H.264/H.265，实际设备的硬件/软件、profile、尺寸及输出能力仍须查询和解码验证；参见[官方格式说明](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/media/avcodec/avcodec-support-formats.md)。SDK 链接库的存在不是安装设备的运行能力证明。

首个能力探针 SHA-256 `5aec712efce6710333df5bdf70b636681773ca71ad5123197a9c6e55fe75586f` 使用官方 SDK 编译；两台专用模拟器从 HDC shell 启动均在进入 main 前加载失败，提示间接依赖缺少 `libark_jsruntime.so`，随后出现 Ark 符号无法解析。HDC 返回 0 不能算探针通过，日志没有 capability/complete 成功记录。系统目录枚举也被拒绝，没有修改系统权限或尝试提权；下一步改在正常权限 HAP 应用上下文验证。原始结果 `target/harmony-system-codec-probe/capabilities-phone.log` 与 `capabilities-pc.log`。此结果不能证明模拟器不支持 H.264/HEVC。

生产选择设计已更新为合格系统硬解 → 合格系统软解 → 已验证 FFmpeg 后备；每项仍需精确格式、生命周期、软件 SIMD 及协议互操作门。当前尚未接入系统 decoder，也未改变既有平台选择策略或 RDP codec 广告。

同日 16:48 改用正常权限 HAP 后，两台查询均成功；都报告系统 H.264 软件解码，未报告 H.264 硬解/HEVC，详见[系统 AVCodec 验证](harmony-system-codec-20261013.md)。先前 shell 加载失败不是设备能力结论。
