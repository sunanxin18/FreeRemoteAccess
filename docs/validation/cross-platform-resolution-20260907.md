# 跨平台本地显示分辨率实现记录

日期：2026-09-07

## 实现范围

- `frd-core` 增加协议无关的 `ResolutionMode`、`DisplayGeometry`、`DisplayIntent` 和 `DisplayPlanner`。
- 默认模式为“显示器原生”，规划器使用本地显示器物理像素，保留 Retina 物理/逻辑换算，不设置 2560×1440 产品上限。
- 面积、纹理尺寸、协议宽高字段和每帧内存预算会做保持比例的安全收敛，并在结果中保留约束原因。
- 桌面 shell 从 winit monitor/window 状态生成物理几何；本地窗口仍使用逻辑点。
- RDP 在协商前消费规划后的尺寸；服务器管理模式的直接协议调用仍回退到 1280×720。已有 Display Control 的精确确认、generation、完整基线和旧表面保留规则未改变。
- Apple HPSS 只在自有虚拟显示启动握手中消费本地意图；Standard/MVS 继续使用服务器管理尺寸。
- 登录页增加显示器原生、工作区、窗口内容、固定尺寸和服务器管理选择，固定选项支持 1280×720、1920×1080、2560×1440、3840×2160 以及手动拖动宽高。

## 自动化证据

已通过：

- `cargo test -p frd-core display --quiet`：7 个规划器测试。
- `cargo test -p frd-protocol-rdp connector --quiet`：初始尺寸、5K 尺寸和服务器管理回退测试。
- `cargo test -p frd-protocol-apple startup_size --lib --quiet`：Apple Standard/MVS 保持 ServerInit，HPSS 使用本地物理意图的测试。
- `cargo test -p frd-shell-desktop display_geometry --lib --quiet`：1×/Retina 缩放因子和窗口逻辑尺寸换算测试。
- `cargo fmt --all -- --check`、`git diff --check`。
- `cargo check` 通过 `frd-core`、`frd-protocol-api`、`frd-protocol-rdp`、
  `frd-protocol-apple`、`frd-ui-model`、`frd-ui-egui`、`frd-app` 和
  `frd-shell-desktop`；macOS debug `.app` staging、包校验和 ad-hoc 签名通过。

## 验证边界

本记录证明规划器、连接请求传递和协议适配器的离线行为。当前没有把任何高于 2560×1440 的尺寸宣称为 Windows 或 Apple 服务端真机互操作结果；RDP/macOS GUI 之前的登录、证书 TOFU、Keychain 保存、完整基线和重连证据仍按各自记录保留。下一次高分辨率真机验证应记录显示器物理尺寸、最终协商尺寸、服务端确认和完整首帧，而不能只依据窗口逻辑尺寸或编译成功。

完整工作区测试已执行；其中未涉及本次改动的 `frd-icon-assets` 派生资源一致性测试仍失败，原因是已提交的 Apple foreground 资源与确定性导出不一致。Windows RDP 全目标交叉检查在本机缺少 MSVC/Windows SDK 与 vcpkg 头文件处停止；`frd-app` 的 Windows target 检查已通过。这两项均单独记录为环境或既有打包状态，不作为本次分辨率实现的代码失败。
