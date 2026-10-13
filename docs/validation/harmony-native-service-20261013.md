# 鸿蒙手机系统原生远控服务证据

日期：2026-10-13。产品边界由用户确认：被控鸿蒙手机也只能使用系统原生服务，不安装配套被控端应用、代理或守护进程。

## 当前结论

**计划中**。HarmonyOS 7 的系统多屏协同确有电脑操作手机的能力，但本次未获得普通第三方客户端可直接使用的完整服务入口、发现、认证、画面和输入协议证据。FreeRemoteDesk 尚不能连接鸿蒙手机作为被控端。这个结论不等于“系统没有远控能力”。

SDK 的屏幕采集和输入控制 API 是本机能力。如果在手机上自行组合采集、网络传输和输入控制，将成为自定义被控端程序，超出产品边界。HDC 仅用于开发和验证自己的客户端，不能成为产品远控协议。

## 已确认的证据及其边界

| 证据 | 已确认 | 尚不能证明 |
|---|---|---|
| [华为鸿蒙电脑通过超级终端协同指南](https://consumer.huawei.com/cn/support/content/zh-cn16048446/)与[操作手机应用和文件](https://consumer.huawei.com/cn/support/content/zh-cn16048421/) | 官方适用版本包含 HarmonyOS 7.0；电脑可以操作手机桌面和应用、键盘输入；系统流程要求账号及 WLAN、星闪/蓝牙等配对条件 | 第三方客户端可调用的服务身份、端口、认证及 wire protocol；不能把官方产品界面流程当成已开放的接口 |
| API 26 `@ohos.multimodalInput.inputEventClient.d.ts` | 存在公开 `createKeyboardController`、`createMouseController`、`createTouchController`，要求 `ohos.permission.CONTROL_DEVICE`，文件无 `@systemapi` | 未申请权限或调用控制器，不能声称普通应用已获授权；显示 ID 是本机对象参数，不是远端网络地址 |
| SDK `native_avscreen_capture.h`；[官方矩形区域录屏指南](https://developer.huawei.com/consumer/cn/doc/doccenter-capabilities/video-supports-rectangular-area-recording) | 本机 screen capture 对象、显示区域、回调及 native window 输出；开始采集含用户 privacy dialog | 现成的远端发现、账号登录、画面传输和输入控制服务 |
| `@ohos.distributedsched.abilityConnectionManager.d.ts`；[官方跨设备 UIAbility 指南](https://developer.huawei.com/consumer/cn/doc/HarmonyOS-Guides/abilityconnectmanager-guidelines) | 同一应用跨设备连接；`PeerInfo` 包含 bundle/module/ability，对端应用需要接受连接 | 无需对端应用配合的整机系统服务 |
| [OpenHarmony 分布式屏幕](https://github.com/openharmony/distributedhardware_distributed_screen/blob/master/README_zh.md) | 分布式硬件框架/InnerKit 下，source 将自己的 Display 投到 sink；经窗口系统使用虚拟屏幕 | 第三方客户端拉取商用手机桌面的通用公开协议 |
| [OpenHarmony 分布式输入](https://github.com/openharmony/distributedhardware_distributed_input/blob/master/README_zh.md) | 组网外设同步及虚拟驱动；组件自身不提供北向接口，由多模输入暴露键鼠穿越 | 任意网络客户端可向商用手机登录并注入输入 |

不能沿用旧版本文档把 API 26 输入控制器一概称为系统应用专属。其公开声明与最终权限授权、真机运行是不同证据层级。

## 本地只读核验范围

- 本机 DevEco SDK `openharmony/toolchains/oh-uni-package.json`：API 26、平台 26.0.0、工具 26.0.0.105 Release。
- 独立手机模拟器 `FreeRemoteDesk Phone API26`：`const.ohos.apiversion=26`、`const.ohos.fullname=OpenHarmony-7.0.0.105`。模拟器自报 OpenHarmony，不能替代商业 HarmonyOS 真机结论。
- `hidumper -ls` 可见 DSoftbus、DeviceManagerService、DistributedDeviceProfile、MultimodalInput、DisplayManagerService、WindowManagerService。SystemAbility 清单不是网络远控端点清单。
- 只读监听端口快照存在动态端口；进程归属因 `/proc` 权限受限未确认。未连接未知端口、扩大权限、采集其他应用画面或发送输入。

本机 SDK 定位（以配置的 SDK 根目录为前缀）：

- `openharmony/ets/api/@ohos.multimodalInput.inputEventClient.d.ts`：第 21 行接口版本与能力；第 87、213、335 行控制器工厂。
- `openharmony/ets/api/permissions.d.ts`：第 3183 行附近的 `CONTROL_DEVICE` 声明。
- `openharmony/native/sysroot/usr/include/multimedia/player_framework/native_avscreen_capture.h`：第 103 行开始采集、第 269 行用户同意流程、第 371 行 native window 输出。
- `openharmony/ets/api/@ohos.distributedsched.abilityConnectionManager.d.ts`：第 21 行同一应用协同说明及 `PeerInfo`/`acceptConnect` 契约。

## 重新开启实现的条件

需要官方公开或合作协议，或者经明确授权、针对真机系统多屏协同的只读协议证据，至少明确原生服务身份、认证材料、画面及输入契约。后续必须验证 FreeRemoteDesk 能独立连接未修改的系统服务；只证明 SDK API 存在、官方协同可用或 HDC 可操作均不满足此条件。

这个缺口不阻止鸿蒙**客户端**开发。客户端连接 Windows 原生 RDP 和 Mac 原生 Screen Sharing/Remote Management 是独立工作项。
