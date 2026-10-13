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

## 本机证据

运行产生的无凭据记录保存在 gitignored 的 `target/harmony-validation/`：

- `environment-20261013.json`：SDK 与工具版本、手机镜像 metadata。
- `emulators-20261013.json`：镜像与实例清单、HDC 连接、两台 guest 的 boot/架构/版本、Rust installed target。
- `setup-20261013.md`：下载、创建、启动和 Rust target 安装命令及结果。

SDK 镜像位于本机配置的 `Library/Huawei/Sdk/system-image/HarmonyOS-7.0.0/`，未纳入 Git。HDC 端口属于这次启动记录，重新启动后需重新列出设备，不能硬编码进构建或产品。
