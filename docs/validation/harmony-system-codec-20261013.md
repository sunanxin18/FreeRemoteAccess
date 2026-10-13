# 鸿蒙系统 AVCodec 验证

日期：2026-10-13。用户要求支持时优先使用系统编解码库。本记录区分 SDK、正常应用能力查询、实际码流解码和生产接线；模拟器不代表真机。

## 运行上下文

官方 API 26 SDK 提供 AVCodec Kit C 接口及 native media 库。[官方支持表](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/media/avcodec/avcodec-support-formats.md)列出 H.264/H.265，并要求查询具体设备能力。最初独立 ELF 从 HDC shell 启动时因间接依赖 `libark_jsruntime.so` 未加载而在 main 前失败，不能据此判断 codec 不支持。

随后在现有工程的隔离副本构建仅含 NAPI 探针的正常权限 HAP，沿用 bundle 和正常开发身份。生产工程、HUKS、连接配置和网络均未被探针调用；原产品包的验证规则没有放宽，探针单独检查两项 ARM64 库（libentry、libc++_shared）及 manifest，并通过官方 sign-app/verify-app。

未签 HAP SHA-256：`3f51e858fedf56563e884576b9e9897690ba59c3e9a56380f738a18f6edda45e`。已签 HAP：`b2f9350e01310cd07d79a5d2fc707424877164fa19a838d587de5de624209307`。手机 16:48:15–16、PC 16:48:16–17 均完成查询、按名称创建/销毁，日志有 `event=complete status=ok` 和 `event=app_complete code=0`。

## 手机与 PC 能力结果

两个专用 API 26 ARM64 模拟器结果相同，分别查询 HARDWARE/SOFTWARE，并交叉检查全量 decoder 列表。

| 能力 | 手机 | PC |
|---|---|---|
| H.264 硬件 decoder | 未报告 | 未报告 |
| H.264 系统软件 decoder | `OH.Media.Codec.Decoder.Video.AVC` | 同左 |
| H.264 profiles | Baseline / Main / High | 同左 |
| 输出像素格式 | I420 / NV12 / NV21 / RGBA | 同左 |
| 宽、高分别报告范围 | 2–5120，分别按 2 对齐 | 同左 |
| low latency feature | false | false |
| output in decoding order feature | true | true |
| H.264 CreateByName / Destroy | 成功 / 0 | 成功 / 0 |
| HEVC 硬件 / 软件 decoder | 均未报告 | 均未报告 |
| AAC 系统软件 decoder | `OH.Media.Codec.Decoder.Audio.AAC`，创建/销毁成功 | 同左 |

单独宽高范围不证明所有二维组合、帧率、吞吐或内存需求均可支持。AAC 列表未报告 profile；采样率 7350–96000 的 13 个离散值、声道范围 1–8 仅为查询结果，不证明 AAC-ELD 解码。模拟器没有 HEVC 不能推断真机也没有。

## 异步 ByteBuffer 实际 fixture

同日 16:51，两台安装相同 fixture HAP（unsigned SHA-256 `b69e07b0f3bbf9a267eec862779a55e4b10ddabf843e8144865294597b5d16a1`，signed `cf10bef2c7962210702b3a4ebc3437c64d984534b835d865fe8e483be9f964ac`），按名称创建系统 AVC 软件 decoder。输入仅为合成样本，参考由已有宿主 FFmpeg 独立解码；合成临时文件运行后删除。

| 合成码流 | 两台输出 | 正确性结论 |
|---|---|---|
| AVC420 256×144 | 55296 bytes NV12 | 与参考逐字节相等 |
| AVC420 2×2 | 6 bytes | 与参考逐字节相等；小图不能判别两种 420 排列 |
| High444 16×16 | 384 bytes，420 大小 | 未保留 444；与两个 420 转换参考也均不相等 |
| High444 256×144 | 55296 bytes，420 大小 | 未保留 444；与两个 420 转换参考也均不相等 |

四种流的 decoder 都报告一个输出帧及 EOS，但只有前两项通过像素比较。`app_complete code=0` 只表示探针遍历完成，不表示所有格式通过。默认输出配置下，探针在送完帧后等候 1.5 秒才发送 EOS，所有帧都在 EOS 提交后输出；这不是实测解码耗时 1.5 秒，也不满足静态远程桌面的及时首帧要求。随后只增加 capability 报告支持的 decoding-order 选项，Configure 返回成功；两台仍在 EOS 后出帧，像素比较仍通过。没有强开报告为 false 的 low-latency feature。

原始日志 `fixtures-app-phone.log`、`fixtures-app-pc.log` 保留 buffer 属性、格式元数据及逐 fixture 比较结果。解码成功后的 Flush/Stop/Destroy 均记录返回值；这仍不是重复 reset、取消或生产 ABI 寿命验收。

解码顺序变体 unsigned SHA-256 `5b4213c0a582d620be169637dda8e0b49e1e1e61029a74d0e4711fed7a2ce2ce`，signed `54093fa86e3a337fa2932d1654558437099e0ec9cda6a559d588ae61a99cf219`；16:54 实际日志为 `fixtures-order-app-phone.log`、`fixtures-order-app-pc.log`。其中 1602–1626 ms 的回调时间包含人为设置的 1500 ms EOS 等候，不能作吞吐或常规解码延迟基准。API20 同步 buffer 接口已另做有界对照，结果如下。

## 同步接口对照及恢复

同步变体 Configure 同时启用 `enable_sync_mode=1` 与 decoding-order；没有注册 callback，每次 QueryInput/Output 最多等候 50 ms，总解码截止 10 秒，硬截止 20 秒。unsigned SHA-256 `de99dd102305a30f74f11557a1ce112a599f0b91c1ec5e2fa38cfdec2fc5be7e`，signed `39df96eab7a33cde4c42510ab80904393419ac74edfaf1b7e33170f0e58dba79`。

16:58 两台同步 API 均成功，均收到一次 stream-changed；两个 AVC420 样本像素仍与参考一致，所有有效帧仍在 EOS 后返回。记录在 `fixtures-sync-app-phone.log`、`fixtures-sync-app-pc.log`。这些三组受限测试共同表明：当前模拟器可通过系统库解出所测 420 图像，但还没有通过“输入单帧后无需新帧或 EOS 就及时输出”的远程桌面要求。不能简单把 callback 换为同步 Query 当成问题已修复，也不能据此宣称所有鸿蒙真机都存在相同行为。

测试结束后已在两台重新安装、启动原 `7452954a…` Task3 签名客户端，官方 Emulator CLI 确认登录表单恢复；临时 shell 探针目录已清理。原产品仍明确显示没有已注册的远程协议，本轮没有真实远程登录。

## 尚待通过的门

两个 AVC420 合成样本已通过实际输出比较；更多裁剪/stride、连续流与低延迟、reset/取消、软件 SIMD 运行证据、平台 backend 接线及真实远程会话仍未完成。H.264 High 不代表 High 4:4:4；AVC444 wire 组合也不能由普通 AVC 查询通过替代。HEVC 在当前两台模拟器不可作为系统候选，后备仍需独立验证。

选择顺序为精确且合格的系统硬解、系统软解、FFmpeg 后备；通过 OHOS 显式策略接入，既有 desktop registry 默认行为不变。

原始日志及隔离探针源码保存在忽略目录 `target/harmony-system-codec-probe/`，其中 `capabilities-app-phone.log`、`capabilities-app-pc.log` 是正常应用的实际查询结果；不与失败的 shell 日志混用。

## 选择策略实现

`e1fb9fa` 在中立媒体 registry 增加显式 `VideoDecoderSelectionPolicy::NativeFirst`：Native 硬解 → Native 软解 → FFmpeg 硬解 → FFmpeg 软解。原 `new()` 继续使用既有 Default 顺序，仍拒绝 Native 软件候选；没有改变任何现有 desktop/OHOS 调用方或注册 codec。

37 项 media-api、49 项共享 runtime 及 1 项文档测试通过，包含默认兼容、精确匹配、ProbeOnly 排除、稳定顺序、创建失败回退及成功后不再创建低优先级候选。首次测试因 PATH rustdoc 与固定 rustc 版本不同在文档步骤失败；显式固定 cargo/rustc/rustdoc 为同一 Rust 1.96 后完整重跑通过。该结果是选择策略回归，不是系统 decoder 已接入或可投入生产的证明。独立代码审查通过，无 Critical/Important/Minor 问题。
